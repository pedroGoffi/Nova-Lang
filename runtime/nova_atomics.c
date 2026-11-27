#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>

/*
 * Nova memory ordering IDs:
 * 0 Relaxed, 1 Acquire, 2 Release, 3 AcqRel, 4 SeqCst.
 * This file is the only native boundary for the public atomic API.
 */
static int nova_atomic_order(int ordering)
{
    switch (ordering) {
        case 0: return __ATOMIC_RELAXED;
        case 1: return __ATOMIC_ACQUIRE;
        case 2: return __ATOMIC_RELEASE;
        case 3: return __ATOMIC_ACQ_REL;
        case 4: return __ATOMIC_SEQ_CST;
        default: abort();
    }
}

static int nova_atomic_load_order(int ordering)
{
    if (ordering == 2 || ordering == 3) abort();
    return nova_atomic_order(ordering);
}

static int nova_atomic_store_order(int ordering)
{
    if (ordering == 1 || ordering == 3) abort();
    return nova_atomic_order(ordering);
}

static int nova_atomic_failure_order(int ordering)
{
    if (ordering == 2 || ordering == 3) abort();
    return nova_atomic_order(ordering);
}

static void nova_atomic_validate_compare_orders(int success, int failure)
{
    /* Failure cannot be Release/AcqRel and cannot be stronger than success. */
    switch (success) {
        case 0:
            if (failure != 0) abort();
            break;
        case 1:
            if (failure != 0 && failure != 1) abort();
            break;
        case 2:
            if (failure != 0) abort();
            break;
        case 3:
            if (failure != 0 && failure != 1) abort();
            break;
        case 4:
            if (failure != 0 && failure != 1 && failure != 4) abort();
            break;
        default:
            abort();
    }
    (void)nova_atomic_failure_order(failure);
    (void)nova_atomic_order(success);
}

bool __nova_atomic_bool_load(uint8_t *address, int ordering)
{
    if (address == NULL) abort();
    return __atomic_load_n(address, nova_atomic_load_order(ordering)) != 0;
}

void __nova_atomic_bool_store(uint8_t *address, bool value, int ordering)
{
    if (address == NULL) abort();
    __atomic_store_n(address, value ? 1u : 0u, nova_atomic_store_order(ordering));
}

bool __nova_atomic_bool_swap(uint8_t *address, bool value, int ordering)
{
    if (address == NULL) abort();
    return __atomic_exchange_n(address, value ? 1u : 0u, nova_atomic_order(ordering)) != 0;
}

bool __nova_atomic_bool_compare_exchange(
    uint8_t *address, bool expected, bool desired,
    int success_ordering, int failure_ordering)
{
    if (address == NULL) abort();
    nova_atomic_validate_compare_orders(success_ordering, failure_ordering);
    uint8_t expected_value = expected ? 1u : 0u;
    return __atomic_compare_exchange_n(
        address, &expected_value, desired ? 1u : 0u, false,
        nova_atomic_order(success_ordering), nova_atomic_failure_order(failure_ordering));
}

bool __nova_atomic_bool_is_lock_free(uint8_t *address)
{
    if (address == NULL) abort();
    return __atomic_is_lock_free(sizeof(uint8_t), address);
}

#define NOVA_DEFINE_ATOMIC_SCALAR(prefix, type) \
    type __nova_atomic_##prefix##_load(type *address, int ordering) { \
        if (address == NULL) abort(); \
        return __atomic_load_n(address, nova_atomic_load_order(ordering)); \
    } \
    void __nova_atomic_##prefix##_store(type *address, type value, int ordering) { \
        if (address == NULL) abort(); \
        __atomic_store_n(address, value, nova_atomic_store_order(ordering)); \
    } \
    type __nova_atomic_##prefix##_swap(type *address, type value, int ordering) { \
        if (address == NULL) abort(); \
        return __atomic_exchange_n(address, value, nova_atomic_order(ordering)); \
    } \
    bool __nova_atomic_##prefix##_compare_exchange( \
        type *address, type expected, type desired, int success_ordering, int failure_ordering) { \
        if (address == NULL) abort(); \
        nova_atomic_validate_compare_orders(success_ordering, failure_ordering); \
        return __atomic_compare_exchange_n( \
            address, &expected, desired, false, \
            nova_atomic_order(success_ordering), nova_atomic_failure_order(failure_ordering)); \
    } \
    bool __nova_atomic_##prefix##_is_lock_free(type *address) { \
        if (address == NULL) abort(); \
        return __atomic_is_lock_free(sizeof(type), address); \
    }

NOVA_DEFINE_ATOMIC_SCALAR(i8, int8_t)
NOVA_DEFINE_ATOMIC_SCALAR(i16, int16_t)
NOVA_DEFINE_ATOMIC_SCALAR(i32, int32_t)
NOVA_DEFINE_ATOMIC_SCALAR(i64, int64_t)
NOVA_DEFINE_ATOMIC_SCALAR(u8, uint8_t)
NOVA_DEFINE_ATOMIC_SCALAR(u16, uint16_t)
NOVA_DEFINE_ATOMIC_SCALAR(u32, uint32_t)
NOVA_DEFINE_ATOMIC_SCALAR(u64, uint64_t)

void *__nova_atomic_ptr_load(void **address, int ordering)
{
    if (address == NULL) abort();
    return __atomic_load_n(address, nova_atomic_load_order(ordering));
}

void __nova_atomic_ptr_store(void **address, void *value, int ordering)
{
    if (address == NULL) abort();
    __atomic_store_n(address, value, nova_atomic_store_order(ordering));
}

void *__nova_atomic_ptr_swap(void **address, void *value, int ordering)
{
    if (address == NULL) abort();
    return __atomic_exchange_n(address, value, nova_atomic_order(ordering));
}

bool __nova_atomic_ptr_compare_exchange(
    void **address, void *expected, void *desired,
    int success_ordering, int failure_ordering)
{
    if (address == NULL) abort();
    nova_atomic_validate_compare_orders(success_ordering, failure_ordering);
    return __atomic_compare_exchange_n(
        address, &expected, desired, false,
        nova_atomic_order(success_ordering), nova_atomic_failure_order(failure_ordering));
}

bool __nova_atomic_ptr_is_lock_free(void **address)
{
    if (address == NULL) abort();
    return __atomic_is_lock_free(sizeof(void *), address);
}
