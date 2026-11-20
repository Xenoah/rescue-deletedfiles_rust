#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <linux/fiemap.h>
#include <linux/fs.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/stat.h>
#include <unistd.h>

static void die(const char *label) { perror(label); exit(1); }

int main(int argc, char **argv) {
    if (argc < 3 || argc > 20) return 2;
    int src[20], dest[20];
    char target[128];
    for (int i = 1; i < argc; i++) {
        src[i] = open(argv[i], O_RDONLY);
        snprintf(target, sizeof(target), "/mnt/sample-%d.bin", i);
        dest[i] = open(target, O_CREAT | O_EXCL | O_RDWR, 0600);
        if (src[i] < 0 || dest[i] < 0) die("open");
    }
    int more;
    do {
        more = 0;
        for (int i = 1; i < argc; i++) {
            unsigned char bytes[4096];
            ssize_t count = read(src[i], bytes, sizeof(bytes));
            if (count < 0) die("read");
            if (!count) continue;
            if (write(dest[i], bytes, count) != count || fsync(dest[i])) die("fsync data");
            more = 1;
        }
    } while (more);
    for (int i = 1; i < argc; i++) {
        struct stat statbuf;
        if (fstat(dest[i], &statbuf)) die("fstat");
        const int limit = 512;
        struct fiemap *map = calloc(1, sizeof(*map) + limit * sizeof(struct fiemap_extent));
        if (!map) die("calloc");
        map->fm_length = ~0ULL; map->fm_extent_count = limit; map->fm_flags = FIEMAP_FLAG_SYNC;
        if (ioctl(dest[i], FS_IOC_FIEMAP, map)) die("FIEMAP");
        printf("FILEMAP %s %lld %u\n", argv[i], (long long)statbuf.st_size, map->fm_mapped_extents);
        for (unsigned j = 0; j < map->fm_mapped_extents; j++) {
            struct fiemap_extent *e = &map->fm_extents[j];
            printf("EXTENT %llu %llu %llu %x\n", (unsigned long long)e->fe_logical,
                   (unsigned long long)e->fe_physical, (unsigned long long)e->fe_length, e->fe_flags);
        }
        free(map); close(src[i]); close(dest[i]);
    }
    return 0;
}
