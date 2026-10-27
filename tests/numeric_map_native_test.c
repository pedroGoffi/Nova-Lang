#include <assert.h>
#include <stdint.h>
#include <stdbool.h>
#include <stdio.h>
#include <inttypes.h>
extern uint64_t __nova_std__internal__map____int64_map_create(void);
extern uint64_t __nova_std__internal__map____int64_map_length(uint64_t);
extern bool __nova_std__internal__map____int64_map_insert(uint64_t,int64_t,int64_t);
extern bool __nova_std__internal__map____int64_map_contains(uint64_t,int64_t);
extern int64_t __nova_std__internal__map____int64_map_get_or(uint64_t,int64_t,int64_t);
extern bool __nova_std__internal__map____int64_map_remove(uint64_t,int64_t);
extern void __nova_std__internal__map____int64_map_clear(uint64_t);
extern void __nova_std__internal__map____int64_map_destroy(uint64_t);
extern int64_t __nova_std__internal__map____int64_map_key_at(uint64_t,uint64_t);
extern int64_t __nova_std__internal__map____int64_map_value_at(uint64_t,uint64_t);
int main(void) {
    uint64_t m=__nova_std__internal__map____int64_map_create();
    for (int64_t i=-5000;i<5000;i++) {
        assert(__nova_std__internal__map____int64_map_insert(m,i,i*2));
    }
    assert(__nova_std__internal__map____int64_map_length(m)==10000);
    int64_t total_keys=0,total_values=0;
    for (uint64_t i=0;i<10000;i++) {
        int64_t k=__nova_std__internal__map____int64_map_key_at(m,i);
        int64_t v=__nova_std__internal__map____int64_map_value_at(m,i);
        assert(v==k*2);
        total_keys+=k;total_values+=v;
    }
    assert(total_keys==-5000 && total_values==-10000);
    for(int64_t i=-5000;i<5000;i++) {
        assert(__nova_std__internal__map____int64_map_contains(m,i));
        assert(__nova_std__internal__map____int64_map_get_or(m,i,123)==i*2);
    }
    assert(!__nova_std__internal__map____int64_map_insert(m,-4999,17));
    assert(__nova_std__internal__map____int64_map_get_or(m,-4999,0)==17);
    assert(__nova_std__internal__map____int64_map_remove(m,-4999));
    assert(!__nova_std__internal__map____int64_map_remove(m,-4999));
    assert(__nova_std__internal__map____int64_map_get_or(m,-4999,88)==88);
    __nova_std__internal__map____int64_map_clear(m);
    assert(__nova_std__internal__map____int64_map_length(m)==0);
    assert(__nova_std__internal__map____int64_map_insert(m,INT64_MIN,INT64_MAX));
    assert(__nova_std__internal__map____int64_map_insert(m,INT64_MAX,INT64_MIN));
    assert(__nova_std__internal__map____int64_map_get_or(m,INT64_MIN,0)==INT64_MAX);
    assert(__nova_std__internal__map____int64_map_get_or(m,INT64_MAX,0)==INT64_MIN);
    __nova_std__internal__map____int64_map_destroy(m);
    for(int i=0;i<10000;i++) {
        uint64_t x=__nova_std__internal__map____int64_map_create();
        __nova_std__internal__map____int64_map_insert(x,i,i);
        __nova_std__internal__map____int64_map_destroy(x);
    }
    puts("numeric_map_native_test: PASS 10k keys + 10k handle cycles");
    return 0;
}
