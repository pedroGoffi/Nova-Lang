#include <stdint.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
void *__nova_json_parse(const char*);
void __nova_json_free(void*);
void __nova_json_require_object(void*);
void *__nova_json_field(void*,const char*);
int64_t __nova_json_signed(void*,int);
uint64_t __nova_json_unsigned(void*,int);
_Bool __nova_json_boolean(void*);
const char *__nova_json_string(void*);
int main(int argc,char **argv){
  if(argc>1){
    if(strcmp(argv[1],"overflow")==0){void*p=__nova_json_parse("{\"id\":256}");__nova_json_unsigned(__nova_json_field(p,"id"),8);return 2;}
    if(strcmp(argv[1],"missing")==0){void*p=__nova_json_parse("{\"id\":2}");__nova_json_field(p,"name");return 2;}
    if(strcmp(argv[1],"duplicate")==0){__nova_json_parse("{\"a\":2,\"a\":3}");return 2;}
    if(strcmp(argv[1],"trailing")==0){__nova_json_parse("{} garbage");return 2;}
    if(strcmp(argv[1],"bool_type")==0){void*p=__nova_json_parse("{\"ok\":\"true\"}");__nova_json_boolean(__nova_json_field(p,"ok"));return 2;}
    if(strcmp(argv[1],"wrong_root")==0){void*p=__nova_json_parse("[]");__nova_json_require_object(p);return 2;}
    if(strcmp(argv[1],"high_surrogate")==0){__nova_json_parse("{\"s\":\"\\uD800\"}");return 2;}
    return 3;
  }
  void*p=__nova_json_parse(" {\"name\":\"Ana \\\"Nova\\\" \\uD83D\\uDE00\",\"age\":42,\"ok\":true,\"nested\":{\"value\":-17},\"extra\":[false,null,3]} ");
  __nova_json_require_object(p);
  if(__nova_json_unsigned(__nova_json_field(p,"age"),8)!=42)abort();
  if(!__nova_json_boolean(__nova_json_field(p,"ok")))abort();
  void*nested=__nova_json_field(p,"nested");
  if(__nova_json_signed(__nova_json_field(nested,"value"),32)!=-17)abort();
  const char*text=__nova_json_string(__nova_json_field(p,"name"));
  __nova_json_free(p);
  if(strcmp(text,"Ana \"Nova\" \xF0\x9F\x98\x80")!=0)abort();
  printf("VALID JSON OK: %s\n",text);
  return 0;
}
