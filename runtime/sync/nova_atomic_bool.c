#ifndef NOVA_ATOMIC_BACKEND_H
#define NOVA_ATOMIC_BACKEND_H

#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>
#include <stdatomic.h>

typedef struct
{
    atomic_bool value;
} NovaAtomicBool;

static memory_order nova_memory_order(int ordering)
{
    switch (ordering)
    {
        case 0: return memory_order_relaxed;
        case 1: return memory_order_acquire;
        case 2: return memory_order_release;
        case 3: return memory_order_acq_rel;
        case 4: return memory_order_seq_cst;
        default: return memory_order_seq_cst;
    }
}

NovaAtomicBool* nova_atomic_bool_create(bool value)
{
    NovaAtomicBool* atomic = malloc(sizeof(NovaAtomicBool));

    if (atomic == NULL)
        return NULL;

    atomic_init(&atomic->value, value);

    return atomic;
}

void nova_atomic_bool_destroy(NovaAtomicBool* self)
{
    free(self);
}

bool nova_atomic_bool_load(
    NovaAtomicBool* self,
    int ordering
)
{
    return atomic_load_explicit(
        &self->value,
        nova_memory_order(ordering)
    );
}

void nova_atomic_bool_store(
    NovaAtomicBool* self,
    bool value,
    int ordering
)
{
    atomic_store_explicit(
        &self->value,
        value,
        nova_memory_order(ordering)
    );
}

bool nova_atomic_bool_swap(
    NovaAtomicBool* self,
    bool value,
    int ordering
)
{
    return atomic_exchange_explicit(
        &self->value,
        value,
        nova_memory_order(ordering)
    );
}

bool nova_atomic_bool_compare_exchange(
    NovaAtomicBool* self,
    bool expected,
    bool desired,
    int success_ordering,
    int failure_ordering
)
{
    return atomic_compare_exchange_strong_explicit(
        &self->value,
        &expected,
        desired,
        nova_memory_order(success_ordering),
        nova_memory_order(failure_ordering)
    );
}

#endif