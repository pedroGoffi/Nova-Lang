// Nova 0.8 bootstrap coroutine runtime: Linux/POSIX stackful contexts.
// Not portable to Windows; not thread-safe; owned opaque handles are mandatory.
#define _XOPEN_SOURCE 700
#define _POSIX_C_SOURCE 200809L
#include <ucontext.h>
#include <stdint.h>
#include <stdbool.h>
#include <stdlib.h>
#include <stdio.h>
#include <string.h>
#include <time.h>

typedef int64_t (*NovaBody)(void *args);
typedef struct NovaCoroutine {
    ucontext_t context;
    ucontext_t *caller;
    NovaBody body;
    void *args;
    unsigned char *stack;
    int64_t incoming;
    int64_t current;
    int done;
    int yielded;
    int running;
    uint64_t handle;
} NovaCoroutine;
#define NOVA_CORO_LIMIT 4096
#define NOVA_CORO_STACK_BYTES (128u * 1024u)
// Generation-stamped handles: low 32 bits are the live slot (1..4095),
// high 32 bits change on every reuse. A stale Task can never adopt a new slot.
// This is intentionally the same handle scheme used by nova_rt.c.
static NovaCoroutine *slots[NOVA_CORO_LIMIT];
static uint32_t generations[NOVA_CORO_LIMIT];
static uint32_t free_slots[NOVA_CORO_LIMIT];
static uint32_t free_count = 0;
static uint32_t next_slot = 1;
static uint32_t slot_of(uint64_t handle) { return (uint32_t)handle; }
static _Thread_local NovaCoroutine *running_coro = NULL;
static _Thread_local ucontext_t outer_context;
static void unschedule(uint64_t handle);

static void __nova_coro_fail(const char *why) {
    fprintf(stderr,"Nova coroutine runtime: %s\n",why);
    abort();
}
static NovaCoroutine *fetch(uint64_t handle) {
    uint32_t slot = slot_of(handle);
    uint32_t generation = (uint32_t)(handle >> 32);
    if (slot == 0 || slot >= NOVA_CORO_LIMIT || generation == 0 ||
        !slots[slot] || generations[slot] != generation)
        __nova_coro_fail("invalid, stale or disposed coroutine handle");
    return slots[slot];
}
static void trampoline(uint32_t low,uint32_t high) {
    uintptr_t address = (uintptr_t)low | ((uintptr_t)high << 32);
    NovaCoroutine *co = (NovaCoroutine *)address;
    int64_t result = co->body(co->args);
    co->current=result;
    co->done=1;
    co->yielded=0;
    co->running=0;
    // The caller has a live stack frame inside __nova_coro_resume().
    if (setcontext(co->caller) != 0) __nova_coro_fail("setcontext failed");
    abort();
}

uint64_t __nova_coro_create(NovaBody fn,void *arguments) {
    if (!fn) __nova_coro_fail("null coroutine function");
    // Reuse a slot only after its previous owned coroutine was dropped.
    // NOVA_CORO_LIMIT now bounds concurrent live handles, not total requests.
    uint32_t slot;
    if (free_count > 0) {
        slot = free_slots[--free_count];
    } else {
        if (next_slot >= NOVA_CORO_LIMIT)
            __nova_coro_fail("concurrent coroutine handle limit reached");
        slot = next_slot++;
    }
    if (generations[slot] == UINT32_MAX)
        __nova_coro_fail("coroutine handle generation exhausted");
    uint32_t generation = ++generations[slot];
    uint64_t handle = ((uint64_t)generation << 32) | slot;
    NovaCoroutine *co=(NovaCoroutine *)calloc(1,sizeof(*co));
    if(!co) __nova_coro_fail("cannot allocate coroutine frame");
    co->stack=malloc(NOVA_CORO_STACK_BYTES);
    if(!co->stack){free(co);__nova_coro_fail("cannot allocate coroutine stack");}
    co->handle=handle;co->body=fn;co->args=arguments;
    if(getcontext(&co->context)!=0) __nova_coro_fail("getcontext failed");
    co->context.uc_stack.ss_sp=co->stack;
    co->context.uc_stack.ss_size=NOVA_CORO_STACK_BYTES;
    co->context.uc_link=NULL;
    uintptr_t addr=(uintptr_t)co;
    makecontext(&co->context,(void (*)(void))trampoline,2,(uint32_t)addr,(uint32_t)(addr>>32));
    slots[slot]=co;
    return handle;
}
int64_t __nova_std__coro____resume(uint64_t handle,int64_t input) {
    NovaCoroutine *co=fetch(handle);
    if (co->done) __nova_coro_fail("cannot resume completed coroutine");
    if (co->running) __nova_coro_fail("cannot recursively resume a running coroutine");
    NovaCoroutine *previous=running_coro;
    co->caller=previous?&previous->context:&outer_context;
    co->incoming=input;
    co->yielded=0;
    co->running=1;
    running_coro=co;
    if(swapcontext(co->caller,&co->context)!=0) __nova_coro_fail("swapcontext resume failed");
    running_coro=previous;
    co->running=0;
    return co->current;
}
int64_t __nova_std__coro____yield_value(int64_t value) {
    NovaCoroutine *co=running_coro;
    if(!co) __nova_coro_fail("yield outside an active coroutine");
    co->current=value;
    co->yielded=1;
    co->running=0;
    if(swapcontext(&co->context,co->caller)!=0) __nova_coro_fail("swapcontext yield failed");
    co->running=1;
    return co->incoming;
}
_Bool __nova_std__coro____done(uint64_t handle){return fetch(handle)->done!=0;}
_Bool __nova_std__coro____yielded(uint64_t handle){return fetch(handle)->yielded!=0;}
void __nova_std__coro____drop(uint64_t handle){
    NovaCoroutine *co=fetch(handle);
    if(co->running || co==running_coro) __nova_coro_fail("cannot drop running coroutine");
    unschedule(handle); // Dropping a pending owned Task also cancels its schedule.
    uint32_t slot = slot_of(handle);
    slots[slot]=NULL;
    free(co->args);free(co->stack);free(co);
    free_slots[free_count++] = slot;
}

#define NOVA_TASKS_LIMIT 1024
static uint64_t scheduled[NOVA_TASKS_LIMIT];
static size_t scheduled_count=0;
static void unschedule(uint64_t handle) {
    for(size_t index=0;index<scheduled_count;) {
        if (scheduled[index] == handle) {
            scheduled[index]=scheduled[--scheduled_count];
        }else{
            index++;
        }
    }
}
static int64_t mono_millis(void){
    struct timespec ts;
    if(clock_gettime(CLOCK_MONOTONIC,&ts)!=0) __nova_coro_fail("clock_gettime failed");
    return (int64_t)ts.tv_sec*1000+ts.tv_nsec/1000000;
}
static void nap(void){const struct timespec ts={0,1000*1000};nanosleep(&ts,NULL);}
static void run_scheduled(uint64_t exclude) {
    for(size_t i=0;i<scheduled_count;) {
        uint64_t handle=scheduled[i];
        if (handle==exclude){i++;continue;}
        NovaCoroutine *co=fetch(handle);
        if (co->done){scheduled[i]=scheduled[--scheduled_count];continue;}
        __nova_std__coro____resume(handle,0);
        if (co->done) scheduled[i]=scheduled[--scheduled_count];else i++;
    }
}
void __nova_std__task____spawn(uint64_t handle) {
    NovaCoroutine *co=fetch(handle);
    if(co->done) __nova_coro_fail("cannot spawn a completed coroutine");
    for(size_t i=0;i<scheduled_count;i++) if(scheduled[i]==handle) return;
    if(scheduled_count>=NOVA_TASKS_LIMIT) __nova_coro_fail("async task limit reached");
    scheduled[scheduled_count++]=handle;
}
int64_t __nova_std__task____block_on(uint64_t handle) {
    NovaCoroutine *co=fetch(handle);
    if(running_coro) __nova_coro_fail("block_on cannot run inside a coroutine; use await");
    while(!co->done) {
        __nova_std__coro____resume(handle,0);
        run_scheduled(handle);
        if(!co->done) nap();
    }
    return co->current;
}
int64_t __nova_std__task____await_value(uint64_t handle) {
    NovaCoroutine *child=fetch(handle);
    if(!running_coro) return __nova_std__task____block_on(handle);
    if(child==running_coro) __nova_coro_fail("a coroutine cannot await itself");
    while(!child->done) {
        __nova_std__coro____resume(handle,0);
        if(!child->done) __nova_std__coro____yield_value(0);
    }
    return child->current;
}
static int64_t sleep_body(void *arg) {
    int64_t delay=*(int64_t *)arg;
    int64_t until=mono_millis()+delay;
    while(mono_millis()<until) __nova_std__coro____yield_value(0);
    return delay;
}
uint64_t __nova_std__task____sleep_ms(uint64_t ms){
    if(ms>3600000u) __nova_coro_fail("sleep_ms limited to one hour");
    int64_t *delay=malloc(sizeof(*delay));
    if(!delay) __nova_coro_fail("cannot allocate timer");
    *delay=(int64_t)ms;
    return __nova_coro_create(sleep_body,delay);
}

// Convenience aliases for one-way generators.
int64_t __nova_std__generator____next(uint64_t h){return __nova_std__coro____resume(h,0);}
_Bool __nova_std__generator____done(uint64_t h){return __nova_std__coro____done(h);}
void __nova_std__generator____drop(uint64_t h){__nova_std__coro____drop(h);}

// Cooperative scheduler operations. These operate on generic coroutine handles
// and intentionally know nothing about HTTP, Express or application protocols.
int32_t __nova_std__task____tick(void) {
    if(running_coro) __nova_coro_fail("scheduler tick cannot run inside a coroutine");
    run_scheduled(0);
    return (int32_t)scheduled_count;
}
void __nova_std__task____run_until_idle(void) {
    if(running_coro) __nova_coro_fail("run_until_idle cannot run inside a coroutine");
    while (scheduled_count != 0) {
        run_scheduled(0);
        if(scheduled_count != 0) nap();
    }
}
int64_t __nova_std__task____yield_now(void) {
    if(!running_coro) __nova_coro_fail("yield_now requires a coroutine");
    return __nova_std__coro____yield_value(0);
}
