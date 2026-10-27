#include <assert.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
extern uint64_t __nova_json_builder_new(void);
extern void __nova_json_builder_append(uint64_t,const char*);
extern const char *__nova_json_builder_finish(uint64_t);
extern int32_t __nova_std__json____release_text(const char *);
int main(void){
    uint64_t b=__nova_json_builder_new();
    __nova_json_builder_append(b,"{\"id\":1}");
    const char*text=__nova_json_builder_finish(b);
    assert(strcmp(text,"{\"id\":1}")==0);
    assert(__nova_std__json____release_text(text)==0);
    assert(__nova_std__json____release_text(text)==-1);
    assert(__nova_std__json____release_text("hello")==-1);
    puts("JSON response buffer released safely");
    return 0;
}
