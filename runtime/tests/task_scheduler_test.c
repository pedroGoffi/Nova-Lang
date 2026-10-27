#include <assert.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>
#include <stdio.h>
extern uint64_t __nova_coro_create(int64_t (*callback)(void *), void *argument);
extern void __nova_std__coro____drop(uint64_t handle);
extern _Bool __nova_std__coro____done(uint64_t handle);
extern int64_t __nova_std__task____yield_now(void);
extern void __nova_std__task____spawn(uint64_t handle);
extern int32_t __nova_std__task____tick(void);
extern void __nova_std__task____run_until_idle(void);
extern int64_t __nova_std__task____block_on(uint64_t handle);
static int executions = 0;
static int64_t worker(void *argument) {
    int64_t *value = argument;
    executions += 1;
    __nova_std__task____yield_now();
    executions += 1;
    return *value + 1;
}
int main(void) {
    int64_t *value = malloc(sizeof(*value));
    assert(value);
    *value = 41;
    uint64_t handle = __nova_coro_create(worker, value);
    __nova_std__task____spawn(handle);
    assert(__nova_std__task____tick() == 1);
    assert(executions == 1);
    __nova_std__task____run_until_idle();
    assert(__nova_std__coro____done(handle));
    assert(executions == 2);
    assert(__nova_std__task____block_on(handle) == 42);
    __nova_std__coro____drop(handle);
    // Dropping a pending Task is cancellation, not a dangling scheduler entry.
    int64_t *cancelled_value=malloc(sizeof(*cancelled_value));
    assert(cancelled_value);
    *cancelled_value=14;
    uint64_t cancelled_handle=__nova_coro_create(worker,cancelled_value);
    __nova_std__task____spawn(cancelled_handle);
    __nova_std__coro____drop(cancelled_handle);
    assert(__nova_std__task____tick()==0);
    assert(executions==2);
    puts("scheduler cooperative task test passed");
    return 0;
}
