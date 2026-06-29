#ifndef RPROXY_HEV_COMPAT_TASK_SYS_SOCKET_H
#define RPROXY_HEV_COMPAT_TASK_SYS_SOCKET_H

#include <winsock2.h>
#include <ws2tcpip.h>
#include <sys/uio.h>

struct msghdr
{
    void *msg_name;
    socklen_t msg_namelen;
    struct iovec *msg_iov;
    size_t msg_iovlen;
    void *msg_control;
    size_t msg_controllen;
    int msg_flags;
};

#ifndef MSG_DONTWAIT
#define MSG_DONTWAIT 0
#endif
#ifndef SHUT_WR
#define SHUT_WR SD_SEND
#endif
#ifndef PF_LOCAL
#define PF_LOCAL AF_INET
#endif

static inline int
rproxy_hev_socketpair (int domain, int type, int protocol, int sv[2])
{
    SOCKET listener = INVALID_SOCKET;
    SOCKET client = INVALID_SOCKET;
    SOCKET server = INVALID_SOCKET;
    struct sockaddr_in addr;
    int addr_len = sizeof (addr);
    int res = -1;

    (void)domain;
    listener = socket (AF_INET, type, protocol);
    if (listener == INVALID_SOCKET)
        goto out;

    memset (&addr, 0, sizeof (addr));
    addr.sin_family = AF_INET;
    addr.sin_addr.s_addr = htonl (INADDR_LOOPBACK);
    addr.sin_port = 0;

    if (bind (listener, (struct sockaddr *)&addr, sizeof (addr)) < 0)
        goto out;
    if (listen (listener, 1) < 0)
        goto out;
    if (getsockname (listener, (struct sockaddr *)&addr, &addr_len) < 0)
        goto out;

    client = socket (AF_INET, type, protocol);
    if (client == INVALID_SOCKET)
        goto out;
    if (connect (client, (struct sockaddr *)&addr, addr_len) < 0)
        goto out;

    server = accept (listener, NULL, NULL);
    if (server == INVALID_SOCKET)
        goto out;

    sv[0] = (int)client;
    sv[1] = (int)server;
    client = INVALID_SOCKET;
    server = INVALID_SOCKET;
    res = 0;

out:
    if (listener != INVALID_SOCKET)
        closesocket (listener);
    if (client != INVALID_SOCKET)
        closesocket (client);
    if (server != INVALID_SOCKET)
        closesocket (server);
    return res;
}

#define socketpair(domain, type, protocol, sv) \
    rproxy_hev_socketpair ((domain), (type), (protocol), (sv))

static inline ssize_t
recvmsg (int fd, struct msghdr *msg, int flags)
{
    WSABUF bufs[msg->msg_iovlen];
    DWORD bytes = 0;
    DWORD wsa_flags = (DWORD)flags;
    int res;

    for (size_t i = 0; i < msg->msg_iovlen; i++) {
        bufs[i].buf = msg->msg_iov[i].iov_base;
        bufs[i].len = (ULONG)msg->msg_iov[i].iov_len;
    }

    res = WSARecvFrom ((SOCKET)fd, bufs, (DWORD)msg->msg_iovlen, &bytes,
                       &wsa_flags, msg->msg_name, &msg->msg_namelen, NULL,
                       NULL);
    msg->msg_flags = (int)wsa_flags;
    return res ? -1 : (ssize_t)bytes;
}

static inline ssize_t
sendmsg (int fd, const struct msghdr *msg, int flags)
{
    WSABUF bufs[msg->msg_iovlen];
    DWORD bytes = 0;
    int res;

    for (size_t i = 0; i < msg->msg_iovlen; i++) {
        bufs[i].buf = msg->msg_iov[i].iov_base;
        bufs[i].len = (ULONG)msg->msg_iov[i].iov_len;
    }

    res = WSASendTo ((SOCKET)fd, bufs, (DWORD)msg->msg_iovlen, &bytes,
                     (DWORD)flags, msg->msg_name, msg->msg_namelen, NULL,
                     NULL);
    return res ? -1 : (ssize_t)bytes;
}

#endif /* RPROXY_HEV_COMPAT_TASK_SYS_SOCKET_H */
