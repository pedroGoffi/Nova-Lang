#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <pthread.h>
int64_t *__nova_std__atomic____new_i64(int64_t initial);
void __nova_std__atomic____free_i64(int64_t *pointer);
int64_t __nova_std__atomic____load_i64(int64_t *pointer,int32_t order);
void __nova_std__atomic____store_i64(int64_t *pointer,int64_t value,int32_t order);
int64_t __nova_std__atomic____swap_i64(int64_t *pointer,int64_t value,int32_t order);
int64_t __nova_std__atomic____fetch_add_i64(int64_t *pointer,int64_t value,int32_t order);
int64_t __nova_std__atomic____fetch_sub_i64(int64_t *pointer,int64_t value,int32_t order);
int32_t __nova_std__atomic____compare_exchange_i64(int64_t *pointer,int64_t expected,int64_t desired,int32_t success,int32_t failure);
static int64_t *shared_counter;
static void *increment_many(void *argument) {
    (void)argument;
    for (int iteration=0; iteration<50000; ++iteration)
        __nova_std__atomic____fetch_add_i64(shared_counter,1,0);
    return NULL;
}
int main(void) {
    shared_counter=__nova_std__atomic____new_i64(0);
    pthread_t workers[6];
    for (int worker_index=0; worker_index<6; ++worker_index) {
        if (pthread_create(&workers[worker_index],NULL,increment_many,NULL)) return 11;
    }
    for (int worker_index=0;worker_index<6;++worker_index)
        if (pthread_join(workers[worker_index],NULL)) return 12;
    if (__nova_std__atomic____load_i64(shared_counter,4)!=300000) return 13;
    if (!__nova_std__atomic____compare_exchange_i64(shared_counter,300000,9,4,1)) return 14;
    if (__nova_std__atomic____compare_exchange_i64(shared_counter,300000,10,4,1)) return 15;
    if (__nova_std__atomic____fetch_sub_i64(shared_counter,4,4)!=9) return 16;
    if (__nova_std__atomic____swap_i64(shared_counter,0,4)!=5) return 17;
    __nova_std__atomic____store_i64(shared_counter,7,2);
    if (__nova_std__atomic____load_i64(shared_counter,1)!=7) return 18;
    __nova_std__atomic____free_i64(shared_counter);
    puts("atomic semantics: OK (6 threads, CAS, add/sub, swap, acquire/release)");
    return 0;
}
