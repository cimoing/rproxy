#ifndef RPROXY_HEV_COMPAT_POLL_H
#define RPROXY_HEV_COMPAT_POLL_H

#include <winsock2.h>

typedef unsigned long nfds_t;

struct pollfd
{
    SOCKET fd;
    short events;
    short revents;
};

#ifndef POLLIN
#define POLLIN 0x0001
#endif
#ifndef POLLOUT
#define POLLOUT 0x0004
#endif
#ifndef POLLERR
#define POLLERR 0x0008
#endif
#ifndef POLLHUP
#define POLLHUP 0x0010
#endif
#ifndef POLLNVAL
#define POLLNVAL 0x0020
#endif

static inline int
poll (struct pollfd *fds, nfds_t nfds, int timeout)
{
    return WSAPoll ((WSAPOLLFD *)fds, (ULONG)nfds, timeout);
}

#endif /* RPROXY_HEV_COMPAT_POLL_H */
