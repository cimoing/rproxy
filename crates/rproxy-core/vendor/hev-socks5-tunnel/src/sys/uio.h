#ifndef RPROXY_HEV_COMPAT_SYS_UIO_H
#define RPROXY_HEV_COMPAT_SYS_UIO_H

#include <stddef.h>
#include <unistd.h>

struct iovec
{
    void *iov_base;
    size_t iov_len;
};

static inline ssize_t
readv (int fd, const struct iovec *iov, int iovcnt)
{
    ssize_t total = 0;

    for (int i = 0; i < iovcnt; i++) {
        ssize_t res = read (fd, iov[i].iov_base, iov[i].iov_len);
        if (res <= 0)
            return total ? total : res;
        total += res;
        if ((size_t)res < iov[i].iov_len)
            break;
    }

    return total;
}

static inline ssize_t
writev (int fd, const struct iovec *iov, int iovcnt)
{
    ssize_t total = 0;

    for (int i = 0; i < iovcnt; i++) {
        ssize_t res = write (fd, iov[i].iov_base, iov[i].iov_len);
        if (res <= 0)
            return total ? total : res;
        total += res;
        if ((size_t)res < iov[i].iov_len)
            break;
    }

    return total;
}

#endif /* RPROXY_HEV_COMPAT_SYS_UIO_H */
