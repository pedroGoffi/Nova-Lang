#include <stdint.h>
#include <stdio.h>
#include <pthread.h>
void *__nova_std__sync____mutex_new(int64_t);
void __nova_std__sync____mutex_lock(void *);
void __nova_std__sync____mutex_unlock(void *);
void __nova_std__sync____mutex_lock_pair(void *,void *);
void __nova_std__sync____mutex_unlock_pair(void *,void *);
int64_t __nova_std__sync____mutex_read(void *);
void __nova_std__sync____mutex_write(void *,int64_t);
void __nova_std__sync____mutex_destroy(void *);
void *__nova_std__thread____thread_start(void (*)(void));
void __nova_std__thread____thread_join(void *);
void __nova_std__thread____thread_destroy(void *);
static void *shared_a,*shared_b;
static void increment_pair(void){
 for (int n=0;n<2500;n++){
  // Reverse acquisition order in the second worker is safe: native order is stable.
  __nova_std__sync____mutex_lock_pair(shared_b,shared_a);
  int64_t current=__nova_std__sync____mutex_read(shared_a);
  __nova_std__sync____mutex_write(shared_a,current+1);
  __nova_std__sync____mutex_unlock_pair(shared_b,shared_a);
 }
}
static void increment_other(void){
 for (int n=0;n<2500;n++){
  __nova_std__sync____mutex_lock_pair(shared_a,shared_b);
  int64_t current=__nova_std__sync____mutex_read(shared_a);
  __nova_std__sync____mutex_write(shared_a,current+1);
  __nova_std__sync____mutex_unlock_pair(shared_a,shared_b);
 }
}
static int finished=0;
static void work(void){finished=1;}
int main(void){
 void *a=__nova_std__sync____mutex_new(100),*b=__nova_std__sync____mutex_new(20);
 __nova_std__sync____mutex_lock(a);
 __nova_std__sync____mutex_write(a,__nova_std__sync____mutex_read(a)+50);
 __nova_std__sync____mutex_unlock(a);
 __nova_std__sync____mutex_lock_pair(b,a);
 __nova_std__sync____mutex_write(a,__nova_std__sync____mutex_read(a)-5);
 __nova_std__sync____mutex_write(b,__nova_std__sync____mutex_read(b)+5);
 __nova_std__sync____mutex_unlock_pair(b,a);
 __nova_std__sync____mutex_lock(a);
 if(__nova_std__sync____mutex_read(a)!=145) return 1;
 __nova_std__sync____mutex_unlock(a);
 __nova_std__sync____mutex_lock(b);
 if(__nova_std__sync____mutex_read(b)!=25) return 2;
 __nova_std__sync____mutex_unlock(b);
 void *thread=__nova_std__thread____thread_start(work);
 __nova_std__thread____thread_join(thread);
 __nova_std__thread____thread_destroy(thread);
 if(finished!=1) return 3;
 shared_a=a;shared_b=b;
 void *first=__nova_std__thread____thread_start(increment_pair);
 void *second=__nova_std__thread____thread_start(increment_other);
 __nova_std__thread____thread_destroy(first);
 __nova_std__thread____thread_destroy(second);
 __nova_std__sync____mutex_lock(a);
 if (__nova_std__sync____mutex_read(a)!=5145) return 4;
 __nova_std__sync____mutex_unlock(a);
 __nova_std__sync____mutex_destroy(a);
 __nova_std__sync____mutex_destroy(b);
 puts("nova sync/thread C smoke: OK");return 0;
}
