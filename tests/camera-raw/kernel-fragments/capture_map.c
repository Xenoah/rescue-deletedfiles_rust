/* Record the kernel's live-file extent addresses on direct Linux volume
 * filesystems. Test harness for disk images mounted inside a disposable guest.
 */
#define _FILE_OFFSET_BITS 64
#include <errno.h>
#include <fcntl.h>
#include <inttypes.h>
#include <linux/fiemap.h>
#include <linux/fs.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/stat.h>
#include <unistd.h>
#define BATCH 128U
struct range {
  uint64_t logical, offset, length;
};
static void die(const char *reason) {
  perror(reason);
  exit(1);
}
static void output(struct range *r) {
  if (r->length)
    printf("RUN %" PRIu64 " %" PRIu64 " %" PRIu64 "\n", r->logical, r->offset,
           r->length);
}
static void add(struct range *r, uint64_t logical, uint64_t offset,
                uint64_t length) {
  if (!length || logical != r->logical + r->length) {
    fprintf(stderr, "noncontiguous logical layout\n");
    exit(1);
  }
  if (r->length && r->offset + r->length == offset) {
    r->length += length;
    return;
  }
  output(r);
  *r = (struct range){logical, offset, length};
}
static int capture_fiemap(int fd, uint64_t size, struct range *r) {
  struct fiemap *f =
      calloc(1, sizeof(*f) + BATCH * sizeof(struct fiemap_extent));
  if (!f)
    die("calloc");
  uint64_t cursor = 0;
  while (cursor < size) {
    memset(f, 0, sizeof(*f) + BATCH * sizeof(struct fiemap_extent));
    f->fm_start = cursor;
    f->fm_length = size - cursor;
    f->fm_flags = FIEMAP_FLAG_SYNC;
    f->fm_extent_count = BATCH;
    if (ioctl(fd, FS_IOC_FIEMAP, f) < 0) {
      if (cursor == 0 && (errno == EOPNOTSUPP || errno == ENOTTY)) {
        free(f);
        return 0;
      }
      die("FIEMAP");
    }
    if (!f->fm_mapped_extents || f->fm_mapped_extents > BATCH) {
      fprintf(stderr, "empty FIEMAP\n");
      exit(1);
    }
    for (unsigned j = 0; j < f->fm_mapped_extents; j++) {
      const struct fiemap_extent *e = &f->fm_extents[j];
      if (e->fe_logical != cursor || e->fe_length == 0 ||
          (e->fe_flags & ~(FIEMAP_EXTENT_LAST | FIEMAP_EXTENT_MERGED |
                           FIEMAP_EXTENT_SHARED))) {
        fprintf(stderr, "unusable FIEMAP range\n");
        exit(1);
      }
      uint64_t len =
          e->fe_length > size - cursor ? size - cursor : e->fe_length;
      add(r, cursor, e->fe_physical, len);
      cursor += len;
    }
  }
  free(f);
  return 1;
}
static void capture_fibmap(int fd, uint64_t size, struct range *r) {
  int unit = 0;
  if (ioctl(fd, FIGETBSZ, &unit) < 0 || unit <= 0)
    die("FIGETBSZ");
  for (uint64_t cursor = 0; cursor < size; cursor += (unsigned)unit) {
    int block = (int)(cursor / (unsigned)unit);
    if (ioctl(fd, FIBMAP, &block) < 0 || block <= 0)
      die("FIBMAP");
    uint64_t len = size - cursor;
    if (len > (unsigned)unit)
      len = (unsigned)unit;
    add(r, cursor, (uint64_t)(unsigned)block * (unsigned)unit, len);
  }
}
int main(int argc, char **argv) {
  if (argc < 2)
    return 2;
  for (int i = 1; i < argc; i++) {
    struct stat st;
    int fd = open(argv[i], O_RDONLY);
    if (fd < 0 || fstat(fd, &st) < 0)
      die("open");
    if (!S_ISREG(st.st_mode) || st.st_size <= 0)
      return 2;
    struct range r = {0};
    printf("FILE %s %" PRIu64 "\n", argv[i], (uint64_t)st.st_size);
    int fiemap = capture_fiemap(fd, (uint64_t)st.st_size, &r);
    if (!fiemap)
      capture_fibmap(fd, (uint64_t)st.st_size, &r);
    output(&r);
    printf("END %s\n", fiemap ? "FIEMAP" : "FIBMAP");
    if (close(fd))
      die("close");
  }
  return ferror(stdout) ? 1 : 0;
}
