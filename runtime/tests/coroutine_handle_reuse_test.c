// Run with clang -std=c11 -O1 -Wall -Wextra -Werror -pthread \
//   runtime/nova_coroutines.c runtime/tests/coroutine_handle_reuse_test.c -o /tmp/nova-coro-reuse-test
#include <assert.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <signal.h>
#include <sys/types.h>
#include <sys/resource.h>
#include <sys/wait.h>
#include <unistd.h>

extern uint64_t __nova_coro_create(int64_t (*callback)(void *), void *argument);
extern void __nova_std__coro____drop(uint64_t handle);
extern int64_t __nova_std__task____block_on(uint64_t handle);
extern void __nova_std__task____spawn(uint64_t handle);
extern int32_t __nova_std__task____tick(void);
extern _Bool __nova_std__coro____done(uint64_t handle);

static int64_t body(void *argument) { return *(int64_t *)argument; }
static uint64_t create(int64_t value) {
    int64_t *argument = malloc(sizeof(*argument));
    assert(argument);
    *argument = value;
    return __nova_coro_create(body, argument);
}
int main(void) {
    uint64_t first = create(10);
    assert(__nova_std__task____block_on(first) == 10);
    __nova_std__coro____drop(first);
    uint64_t replacement = create(11);
    assert(first != replacement);
    assert((uint32_t)first == (uint32_t)replacement); // reclaimed slot
    assert((first >> 32) != (replacement >> 32));     // bumped generation
    // The stale handle must be rejected even though the slot has been reused.
    pid_t child = fork();
    assert(child >= 0);
    if (child == 0) {
        const struct rlimit no_core = { 0, 0 };
        (void)setrlimit(RLIMIT_CORE, &no_core);
        (void)freopen("/dev/null", "w", stderr);
        (void)__nova_std__coro____done(first);
        _exit(0);
    }
    int status = 0;
    assert(waitpid(child, &status, 0) == child);
    assert(WIFSIGNALED(status) && WTERMSIG(status) == SIGABRT);
    assert(__nova_std__task____block_on(replacement) == 11);
    __nova_std__coro____drop(replacement);

    // Must be able to create > 4096 tasks over the server lifetime.
    for (int64_t i = 0; i < 15000; ++i) {
        uint64_t task = create(i);
        assert(__nova_std__task____block_on(task) == i);
        __nova_std__coro____drop(task);
    }

    // Also support simultaneous (distinct) live handles and correct schedule cleanup.
    uint64_t pending[64];
    for (int64_t i = 0; i < 64; ++i) {
        pending[i] = create(i + 100);
        __nova_std__task____spawn(pending[i]);
    }
    assert(__nova_std__task____tick() == 0);
    for (int64_t i = 0; i < 64; ++i) {
        assert(__nova_std__task____block_on(pending[i]) == i + 100);
        __nova_std__coro____drop(pending[i]);
    }
    assert(__nova_std__task____tick() == 0);
    puts("Nova coroutine generation/reuse regression passed (15000+ owners)");
    return 0;
}
