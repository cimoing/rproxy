#ifndef RPROXY_HEV_COMPAT_SYS_IOCTL_H
#define RPROXY_HEV_COMPAT_SYS_IOCTL_H

#include <winsock2.h>

#define ioctl(fd, request, argp) ioctlsocket ((SOCKET)(fd), (long)(request), (u_long *)(argp))

#endif /* RPROXY_HEV_COMPAT_SYS_IOCTL_H */
