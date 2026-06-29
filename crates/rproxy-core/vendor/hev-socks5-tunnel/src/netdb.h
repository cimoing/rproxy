#ifndef RPROXY_HEV_COMPAT_NETDB_H
#define RPROXY_HEV_COMPAT_NETDB_H

#include <winsock2.h>
#include <ws2tcpip.h>

#ifndef EAI_SYSTEM
#define EAI_SYSTEM EAI_FAIL
#endif

#endif /* RPROXY_HEV_COMPAT_NETDB_H */
