/* Generic C11 atomic primitives for Nova. All orderings are validated.
 * Atomic allocations are opaque and must be uniquely owned by Nova structs.
 */
#include <stdatomic.h>
#include <stdint.h>
#include <stdlib.h>
#include <stdio.h>

typedef _Atomic(int64_t) NovaAtomicI64;

static void atomic_error(const char *message) {
    fprintf(stderr,"nova atomic error: %s\n",message);
    abort();
}
static NovaAtomicI64 *as_atomic(int64_t *pointer) {
    if (!pointer) atomic_error("null atomic pointer");
    return (NovaAtomicI64 *)(void *)pointer;
}
static memory_order ordering(int32_t value) {
    switch(value) {
        case 0: return memory_order_relaxed;
        case 1: return memory_order_acquire;
        case 2: return memory_order_release;
        case 3: return memory_order_acq_rel;
        case 4: return memory_order_seq_cst;
        default: atomic_error("invalid memory ordering");
    }
    return memory_order_seq_cst;
}
static memory_order checked_load_order(int32_t value) {
    memory_order order=ordering(value);
    if (order==memory_order_release || order==memory_order_acq_rel)
        atomic_error("load cannot use release or acq_rel ordering");
    return order;
}
static memory_order checked_store_order(int32_t value) {
    memory_order order=ordering(value);
    if (order==memory_order_acquire || order==memory_order_acq_rel)
        atomic_error("store cannot use acquire or acq_rel ordering");
    return order;
}
int64_t *__nova_std__atomic____new_i64(int64_t initial_value) {
    NovaAtomicI64 *result=malloc(sizeof(*result));
    if (!result) atomic_error("out of memory");
    atomic_init(result, initial_value);
    return (int64_t *)(void *)result;
}
void __nova_std__atomic____free_i64(int64_t *pointer) {
    free(as_atomic(pointer));
}
int64_t __nova_std__atomic____load_i64(int64_t *pointer,int32_t order) {
    return atomic_load_explicit(as_atomic(pointer),checked_load_order(order));
}
void __nova_std__atomic____store_i64(int64_t *pointer,int64_t value,int32_t order) {
    atomic_store_explicit(as_atomic(pointer),value,checked_store_order(order));
}
int64_t __nova_std__atomic____swap_i64(int64_t *pointer,int64_t value,int32_t order) {
    return atomic_exchange_explicit(as_atomic(pointer),value,ordering(order));
}
int64_t __nova_std__atomic____fetch_add_i64(int64_t *pointer,int64_t amount,int32_t order) {
    return atomic_fetch_add_explicit(as_atomic(pointer),amount,ordering(order));
}
int64_t __nova_std__atomic____fetch_sub_i64(int64_t *pointer,int64_t amount,int32_t order) {
    return atomic_fetch_sub_explicit(as_atomic(pointer),amount,ordering(order));
}
int64_t __nova_std__atomic____fetch_and_i64(int64_t *pointer,int64_t mask,int32_t order) {
    return atomic_fetch_and_explicit(as_atomic(pointer),mask,ordering(order));
}
int64_t __nova_std__atomic____fetch_or_i64(int64_t *pointer,int64_t mask,int32_t order) {
    return atomic_fetch_or_explicit(as_atomic(pointer),mask,ordering(order));
}
int64_t __nova_std__atomic____fetch_xor_i64(int64_t *pointer,int64_t mask,int32_t order) {
    return atomic_fetch_xor_explicit(as_atomic(pointer),mask,ordering(order));
}
int32_t __nova_std__atomic____compare_exchange_i64(int64_t *pointer,int64_t expected,int64_t desired,int32_t success,int32_t failure) {
    memory_order success_order=ordering(success);
    memory_order failure_order=checked_load_order(failure);
    /* Forbid failure ordering stronger than success per C11 CAS rules. */
    if ((success_order==memory_order_relaxed && failure_order!=memory_order_relaxed)
        || (success_order==memory_order_release && failure_order!=memory_order_relaxed)
        || (success_order==memory_order_acquire && failure_order==memory_order_seq_cst)
        || (success_order==memory_order_acq_rel && failure_order==memory_order_seq_cst))
        atomic_error("failure order stronger than success order");
    return atomic_compare_exchange_strong_explicit(as_atomic(pointer),&expected,desired,success_order,failure_order) ? 1 : 0;
}
