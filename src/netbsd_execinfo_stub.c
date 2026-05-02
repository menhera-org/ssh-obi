#include <stddef.h>

/* cross-rs' NetBSD image lacks libexecinfo; Rust only needs link symbols here. */
int
backtrace(void **buffer, int size)
{
    (void)buffer;
    (void)size;
    return 0;
}

char **
backtrace_symbols(void *const *buffer, int size)
{
    (void)buffer;
    (void)size;
    return NULL;
}

void
backtrace_symbols_fd(void *const *buffer, int size, int fd)
{
    (void)buffer;
    (void)size;
    (void)fd;
}
