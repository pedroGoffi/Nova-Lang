#define _XOPEN_SOURCE 700
#define _POSIX_C_SOURCE 200809L
#include <ucontext.h>
#include <stdint.h>
#include <stdbool.h>
#include <stdlib.h>
#include <stdio.h>
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
} NovaCoroutine;

#define NOVA_CORO_LIMIT 4096
#define NOVA_CORO_STACK_BYTES (128u * 1024u)
static NovaCoroutine *slots[NOVA_CORO_LIMIT];
static uint32_t generations[NOVA_CORO_LIMIT];
static uint32_t free_slots[NOVA_CORO_LIMIT];
static uint32_t free_count;
static uint32_t next_slot = 1;
static _Thread_local NovaCoroutine *running_coro;
static _Thread_local ucontext_t outer_context;

static void fail(const char *message) {
    fprintf(stderr, "Nova coroutine runtime: %s\n", message);
    abort();
}
static NovaCoroutine *fetch(uint64_t handle) {
    uint32_t slot = (uint32_t)handle;
    uint32_t generation = (uint32_t)(handle >> 32);
    if (!slot || slot >= NOVA_CORO_LIMIT || !generation || !slots[slot]
        || generations[slot] != generation) fail("invalid coroutine handle");
    return slots[slot];
}
static void trampoline(uint32_t low, uint32_t high) {
    uintptr_t address = (uintptr_t)low | ((uintptr_t)high << 32);
    NovaCoroutine *co = (NovaCoroutine *)address;
    co->current = co->body(co->args);
    co->done = 1;
    co->yielded = 0;
    co->running = 0;
    if (setcontext(co->caller) != 0) fail("setcontext failed");
    abort();
}
uint64_t __nova_coro_create(NovaBody body, void *arguments) {
    if (!body) fail("null coroutine body");
    uint32_t slot = free_count ? free_slots[--free_count] : next_slot++;
    if (slot >= NOVA_CORO_LIMIT) fail("coroutine handle limit reached");
    uint32_t generation = ++generations[slot];
    if (!generation) fail("coroutine generation exhausted");
    NovaCoroutine *co = calloc(1, sizeof(*co));
    if (!co) fail("cannot allocate coroutine");
    co->stack = malloc(NOVA_CORO_STACK_BYTES);
    if (!co->stack) fail("cannot allocate coroutine stack");
    co->body = body;
    co->args = arguments;
    if (getcontext(&co->context) != 0) fail("getcontext failed");
    co->context.uc_stack.ss_sp = co->stack;
    co->context.uc_stack.ss_size = NOVA_CORO_STACK_BYTES;
    co->context.uc_link = NULL;
    uintptr_t address = (uintptr_t)co;
    makecontext(&co->context, (void (*)(void))trampoline, 2,
                (uint32_t)address, (uint32_t)(address >> 32));
    slots[slot] = co;
    return ((uint64_t)generation << 32) | slot;
}
int64_t __nova_std__coro____resume(uint64_t handle, int64_t input) {
    NovaCoroutine *co = fetch(handle);
    if (co->done || co->running) fail("cannot resume coroutine");
    NovaCoroutine *previous = running_coro;
    co->caller = previous ? &previous->context : &outer_context;
    co->incoming = input;
    co->yielded = 0;
    co->running = 1;
    running_coro = co;
    if (swapcontext(co->caller, &co->context) != 0) fail("swapcontext failed");
    running_coro = previous;
    co->running = 0;
    return co->current;
}
int64_t __nova_std__coro____yield_value(int64_t value) {
    NovaCoroutine *co = running_coro;
    if (!co) fail("yield outside coroutine");
    co->current = value;
    co->yielded = 1;
    co->running = 0;
    if (swapcontext(&co->context, co->caller) != 0) fail("yield swap failed");
    co->running = 1;
    return co->incoming;
}
_Bool __nova_std__coro____done(uint64_t handle) { return fetch(handle)->done != 0; }
_Bool __nova_std__coro____yielded(uint64_t handle) { return fetch(handle)->yielded != 0; }
void __nova_std__coro____drop(uint64_t handle) {
    NovaCoroutine *co = fetch(handle);
    if (co->running) fail("cannot drop running coroutine");
    uint32_t slot = (uint32_t)handle;
    slots[slot] = NULL;
    free(co->args);
    free(co->stack);
    free(co);
    free_slots[free_count++] = slot;
}

#define NOVA_TASKS_LIMIT 1024
static uint64_t scheduled[NOVA_TASKS_LIMIT];
static size_t scheduled_count;
static int64_t mono_millis(void) {
    struct timespec ts;
    if (clock_gettime(CLOCK_MONOTONIC, &ts) != 0) fail("clock_gettime failed");
    return (int64_t)ts.tv_sec * 1000 + ts.tv_nsec / 1000000;
}
static void nap(void) { struct timespec ts = {0, 1000000}; nanosleep(&ts, NULL); }
static void run_scheduled(uint64_t exclude) {
    for (size_t i = 0; i < scheduled_count;) {
        uint64_t handle = scheduled[i];
        if (handle == exclude) { ++i; continue; }
        NovaCoroutine *co = fetch(handle);
        if (co->done) { scheduled[i] = scheduled[--scheduled_count]; continue; }
        __nova_std__coro____resume(handle, 0);
        if (co->done) scheduled[i] = scheduled[--scheduled_count]; else ++i;
    }
}
void __nova_std__task____spawn(uint64_t handle) {
    fetch(handle);
    for (size_t i = 0; i < scheduled_count; ++i) if (scheduled[i] == handle) return;
    if (scheduled_count == NOVA_TASKS_LIMIT) fail("task limit reached");
    scheduled[scheduled_count++] = handle;
}
int64_t __nova_std__task____block_on(uint64_t handle) {
    NovaCoroutine *co = fetch(handle);
    if (running_coro) fail("block_on inside coroutine; use await");
    if (co->done) return co->current;
    while (!co->done) { __nova_std__coro____resume(handle, 0); run_scheduled(handle); if (!co->done) nap(); }
    return co->current;
}
int64_t __nova_std__task____await_value(uint64_t handle) {
    NovaCoroutine *child = fetch(handle);
    if (child->done) return child->current;
    if (!running_coro) return __nova_std__task____block_on(handle);
    if (child == running_coro) fail("coroutine cannot await itself");
    while (!child->done) { __nova_std__coro____resume(handle, 0); if (!child->done) __nova_std__coro____yield_value(0); }
    return child->current;
}
static int64_t sleep_body(void *arg) {
    int64_t delay = *(int64_t *)arg;
    int64_t until = mono_millis() + delay;
    while (mono_millis() < until) __nova_std__coro____yield_value(0);
    return delay;
}
uint64_t __nova_std__task____sleep_ms(uint64_t ms) {
    int64_t *delay = malloc(sizeof(*delay));
    if (!delay) fail("cannot allocate timer");
    *delay = (int64_t)ms;
    return __nova_coro_create(sleep_body, delay);
}
int64_t __nova_std__generator____next(uint64_t handle) { return __nova_std__coro____resume(handle, 0); }
_Bool __nova_std__generator____done(uint64_t handle) { return __nova_std__coro____done(handle); }
void __nova_std__generator____drop(uint64_t handle) { __nova_std__coro____drop(handle); }
int32_t __nova_std__task____tick(void) { run_scheduled(0); return (int32_t)scheduled_count; }
void __nova_std__task____run_until_idle(void) { while (scheduled_count) { run_scheduled(0); if (scheduled_count) nap(); } }
int64_t __nova_std__task____yield_now(void) { return __nova_std__coro____yield_value(0); }
