#define _POSIX_C_SOURCE 200809L
/* Owned mutexes and native threads for Nova. Guard operations validate that
 * the current thread holds the lock. Inverse joint acquisition is serialized
 * by a stable pointer order. Compile-time detection is handled separately. */
#include <pthread.h>
#include <stdint.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <errno.h>

static void concurrency_error(const char *message, int code) {
    fprintf(stderr,"runtime[NOVA-C100]: %s (pthread error %d)\n",message,code);
    fflush(stderr);
    abort();
}
static void check(int code,const char *message) {
    if (code) concurrency_error(message,code);
}
static void *allocated(size_t size) {
    void *p=calloc(1,size);
    if (!p) concurrency_error("out of memory",ENOMEM);
    return p;
}
typedef struct {pthread_mutex_t mutex; int64_t value;} NovaMutexI64;

/* Locks tracked only for the calling thread. TLS avoids a data race when
 * checking whether the guard owns a mutex on another thread. */
typedef struct NovaHeld {NovaMutexI64 *mutex; struct NovaHeld *next;} NovaHeld;
static _Thread_local NovaHeld *held_mutexes;
static bool holds(NovaMutexI64 *mutex) {
    for (NovaHeld *node=held_mutexes;node;node=node->next)
        if(node->mutex==mutex) return true;
    return false;
}
static void add_held(NovaMutexI64 *mutex) {
    if (holds(mutex)) concurrency_error("recursive lock acquisition",EDEADLK);
    NovaHeld *node=allocated(sizeof(*node));
    node->mutex=mutex;
    node->next=held_mutexes;
    held_mutexes=node;
}
static void remove_held(NovaMutexI64 *mutex) {
    NovaHeld **link=&held_mutexes;
    while (*link && (*link)->mutex!=mutex) link=&(*link)->next;
    if (!*link) concurrency_error("unlock without guard ownership",EPERM);
    NovaHeld *node=*link;
    *link=node->next;
    free(node);
}
void *__nova_std__sync____mutex_new(int64_t value) {
    NovaMutexI64 *resource=allocated(sizeof(*resource));
    pthread_mutexattr_t attributes;
    check(pthread_mutexattr_init(&attributes),"mutex attr init");
    check(pthread_mutexattr_settype(&attributes,PTHREAD_MUTEX_ERRORCHECK),"set errorcheck mutex");
    check(pthread_mutex_init(&resource->mutex,&attributes),"mutex init");
    check(pthread_mutexattr_destroy(&attributes),"mutex attr destroy");
    resource->value=value;
    return resource;
}
void __nova_std__sync____mutex_destroy(void *handle) {
    NovaMutexI64 *resource=handle;
    if (!resource) concurrency_error("null mutex",EINVAL);
    check(pthread_mutex_destroy(&resource->mutex),"destroy locked or invalid mutex");
    free(resource);
}
void __nova_std__sync____mutex_lock(void *handle) {
    NovaMutexI64 *resource=handle;
    if (!resource) concurrency_error("null mutex",EINVAL);
    if (holds(resource)) concurrency_error("recursive mutex lock",EDEADLK);
    check(pthread_mutex_lock(&resource->mutex),"mutex lock");
    add_held(resource);
}
void __nova_std__sync____mutex_unlock(void *handle) {
    NovaMutexI64 *resource=handle;
    if (!resource) concurrency_error("null mutex",EINVAL);
    remove_held(resource);
    check(pthread_mutex_unlock(&resource->mutex),"mutex unlock");
}
void __nova_std__sync____mutex_lock_pair(void *first,void *second) {
    if (first==second) concurrency_error("joint acquisition aliases the same mutex",EDEADLK);
    /* uintptr_t gives stable numeric ordering for this runtime platform. */
    if ((uintptr_t)first < (uintptr_t)second) {
        __nova_std__sync____mutex_lock(first);
        __nova_std__sync____mutex_lock(second);
    } else {
        __nova_std__sync____mutex_lock(second);
        __nova_std__sync____mutex_lock(first);
    }
}
void __nova_std__sync____mutex_unlock_pair(void *first,void *second) {
    if ((uintptr_t)first < (uintptr_t)second) {
        __nova_std__sync____mutex_unlock(second);
        __nova_std__sync____mutex_unlock(first);
    } else {
        __nova_std__sync____mutex_unlock(first);
        __nova_std__sync____mutex_unlock(second);
    }
}
int64_t __nova_std__sync____mutex_read(void *handle) {
    NovaMutexI64 *resource=handle;
    if (!resource || !holds(resource)) concurrency_error("read requires an active guard",EPERM);
    return resource->value;
}
void __nova_std__sync____mutex_write(void *handle,int64_t value) {
    NovaMutexI64 *resource=handle;
    if (!resource || !holds(resource)) concurrency_error("write requires an active guard",EPERM);
    resource->value=value;
}

typedef struct { pthread_t tid; bool joined; } NovaThread;
static void *thread_trampoline(void *context) {
    void (*callback)(void)=context;
    callback();
    return NULL;
}
void *__nova_std__thread____thread_start(void (*callback)(void)) {
    if (!callback) concurrency_error("null thread callback",EINVAL);
    NovaThread *thread=allocated(sizeof(*thread));
    check(pthread_create(&thread->tid,NULL,thread_trampoline,(void *)callback),"thread start");
    return thread;
}
void __nova_std__thread____thread_join(void *handle) {
    NovaThread *thread=handle;
    if (!thread) concurrency_error("null thread handle",EINVAL);
    if (thread->joined) return;
    if (pthread_equal(thread->tid,pthread_self())) concurrency_error("thread attempted to join itself",EDEADLK);
    check(pthread_join(thread->tid,NULL),"thread join");
    thread->joined=true;
}
void __nova_std__thread____thread_destroy(void *handle) {
    if (!handle) return;
    __nova_std__thread____thread_join(handle);
    free(handle);
}
