#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <unistd.h>

#define MAX_SHM_BYTES (64u * 1024u * 1024u)

static int valid_name(const char *name) {
  if (!name || name[0] != '/' || name[1] == '\0') return 0;
  for (const unsigned char *p = (const unsigned char *)name + 1; *p; ++p) {
    if (*p == '/' || *p == '\\' || *p < 0x21 || *p > 0x7e) return 0;
  }
  return 1;
}

int main(int argc, char **argv) {
  if ((argc != 2 && argc != 3) || !valid_name(argv[1])) {
    fprintf(stderr, "usage: tap-shm-read /SAFE_NAME [BYTE_LENGTH]\n");
    return 2;
  }
  const char *name = argv[1];
  int fd = shm_open(name, O_RDONLY, 0);
  if (fd < 0) {
    fprintf(stderr, "shm_open: %s\n", strerror(errno));
    return 1;
  }
  struct stat info;
  if (fstat(fd, &info) != 0 || info.st_size < 0 ||
      (uint64_t)info.st_size > MAX_SHM_BYTES) {
    fprintf(stderr, "invalid shared-memory size\n");
    close(fd);
    return 1;
  }
  size_t size = (size_t)info.st_size;
  if (argc == 3) {
    char *end = NULL;
    unsigned long long requested = strtoull(argv[2], &end, 10);
    if (!end || *end != '\0' || requested > size || requested > MAX_SHM_BYTES) {
      fprintf(stderr, "invalid requested shared-memory length\n");
      close(fd);
      return 1;
    }
    size = (size_t)requested;
  }
  void *memory = size == 0 ? NULL : mmap(NULL, size, PROT_READ, MAP_SHARED, fd, 0);
  if (size > 0 && memory == MAP_FAILED) {
    fprintf(stderr, "shared-memory mmap failed\n");
    close(fd);
    return 1;
  }
  if (size > 0 && fwrite(memory, 1, size, stdout) != size) {
    munmap(memory, size);
    close(fd);
    return 1;
  }
  if (size > 0) munmap(memory, size);
  close(fd);
  if (shm_unlink(name) != 0) {
    fprintf(stderr, "shm_unlink: %s\n", strerror(errno));
    return 1;
  }
  return fflush(stdout) == 0 ? 0 : 1;
}
