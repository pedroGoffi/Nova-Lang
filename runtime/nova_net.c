// Generic, bounded TCP byte-stream adapter for Nova (no HTTP semantics).
// Native operations are deliberately protocol-agnostic; application protocol
// parsing, routing and dispatch are implemented by Nova packages.
#define _POSIX_C_SOURCE 200809L
#include <arpa/inet.h>
#include <errno.h>
#include <fcntl.h>
#include <netinet/in.h>
#include <poll.h>
#include <stdint.h>
#include <stddef.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/types.h>
#include <unistd.h>

extern void __nova_std__string____push(uint64_t handle, const char *text);

static int nonblock(int fd) {
    int flags=fcntl(fd,F_GETFL,0);
    return flags>=0 ? fcntl(fd,F_SETFL,flags|O_NONBLOCK) : -1;
}
int32_t __nova_std__net____tcp_listen(uint64_t port) {
    if (port<1||port>65535) return -1;
    int fd=socket(AF_INET,SOCK_STREAM,0);
    if(fd<0) return -1;
    int yes=1;
    (void)setsockopt(fd,SOL_SOCKET,SO_REUSEADDR,&yes,sizeof(yes));
    struct sockaddr_in addr={0};
    addr.sin_family=AF_INET;
    // Stage-0 safety: loopback only. Exposing interfaces requires an explicit
    // future opt-in and network threat model.
    addr.sin_addr.s_addr=htonl(INADDR_LOOPBACK);
    addr.sin_port=htons((uint16_t)port);
    if(nonblock(fd)<0 || bind(fd,(struct sockaddr*)&addr,sizeof(addr))<0 || listen(fd,64)<0){close(fd);return -1;}
    return fd;
}
int32_t __nova_std__net____tcp_accept(int32_t listener) {
    if(listener<0)return -1;
    int fd=accept(listener,NULL,NULL);
    if(fd<0)return (errno==EAGAIN||errno==EWOULDBLOCK||errno==EINTR) ? -2 : -1;
    if(nonblock(fd)<0){close(fd);return -1;}
    return fd;
}
int64_t __nova_std__net____tcp_read(int32_t fd, uint64_t out, uint64_t max_bytes) {
    if(fd<0||out==0||max_bytes==0||max_bytes>16384)return -1;
    char buf[16385];
    ssize_t n=recv(fd,buf,(size_t)max_bytes,0);
    if(n<0)return (errno==EAGAIN||errno==EWOULDBLOCK||errno==EINTR) ? -2 : -1;
    // Nova String is text, not an arbitrary byte buffer. Binary data/NUL is
    // rejected. A byte-buffer API is a separate future addition.
    if(memchr(buf,'\0',(size_t)n))return -3;
    buf[n]=0;
    if(n>0)__nova_std__string____push(out,buf);
    return n;
}
int64_t __nova_std__net____tcp_write(int32_t fd,const char *text) {
    if(fd<0||!text)return -1;
    size_t len=strnlen(text,1048577);
    if(len>1048576)return -1;
    size_t sent=0;
    while(sent<len) {
        ssize_t n=send(fd,text+sent,len-sent,MSG_NOSIGNAL);
        if(n<0) {
            if(errno==EAGAIN||errno==EWOULDBLOCK||errno==EINTR) {
                struct pollfd pollfd={.fd=fd,.events=POLLOUT};
                if(poll(&pollfd,1,1000)>0)continue;
            }
            return -1;
        }
        if(n==0)return -1;
        sent+=(size_t)n;
    }
    return (int64_t)sent;
}
void __nova_std__net____tcp_close(int32_t fd) {if(fd>=0)close(fd);}
