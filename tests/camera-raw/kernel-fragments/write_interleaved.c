/* QA guest workload. Write pieces of real files in round-robin order and fsync
 * each piece. */
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/types.h>
#include <unistd.h>
#ifndef WRITE_CHUNK_BYTES
#define WRITE_CHUNK_BYTES 65536
#endif
static void fail(const char *name) {
  perror(name);
  exit(1);
}
int main(int argc, char **argv) {
  if (argc < 3)
    return 2;
  int n = argc - 2, in[n], out[n];
  unsigned char buf[WRITE_CHUNK_BYTES];
  for (int i = 0; i < n; i++) {
    char dest[PATH_MAX];
    char *file = strrchr(argv[i + 2], '/');
    if (file == NULL || snprintf(dest, sizeof dest, "%s/%s", argv[1],
                                 file + 1) >= (int)sizeof dest)
      return 2;
    if ((in[i] = open(argv[i + 2], O_RDONLY)) < 0 ||
        (out[i] = open(dest, O_WRONLY | O_CREAT | O_EXCL, 0644)) < 0)
      fail(dest);
  }
  int active = n;
  while (active)
    for (int i = 0; i < n; i++) {
      if (in[i] < 0)
        continue;
      ssize_t z = read(in[i], buf, sizeof buf);
      if (z < 0)
        fail("read");
      if (!z) {
        close(in[i]);
        close(out[i]);
        in[i] = -1;
        active--;
        continue;
      }
      ssize_t wrote = 0;
      while (wrote < z) {
        ssize_t k = write(out[i], buf + wrote, (size_t)(z - wrote));
        if (k <= 0)
          fail("write");
        wrote += k;
      }
      if (fsync(out[i]) < 0)
        fail("fsync");
    }
  return 0;
}
