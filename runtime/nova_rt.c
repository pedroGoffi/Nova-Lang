#define _POSIX_C_SOURCE 200809L
/* Nova 0.5 MVP runtime: checked, generational, single-threaded resource handles.
 * C11, LP64 target.  Zero-sized allocations are represented by non-null storage.
 * This is a runtime safety layer, NOT a static Rust-style borrow checker.
 */
#include <stdint.h>
#include <stddef.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <inttypes.h>
#include <limits.h>
#include <errno.h>
#include <unistd.h>
#if defined(__linux__)
#include <execinfo.h>
#endif

#if SIZE_MAX != UINT64_MAX
#error "Nova MVP runtime requires a 64-bit target"
#endif

typedef enum { FREE=0, OWN_STRING, BUILDER, BUFFER, VECTOR_I64, VECTOR_U8, ARENA, MAP_I64, MAP_NUMERIC } Kind;
// Open-addressing table entries. Key memory belongs to the table.
// state: 0 = unused, 1 = occupied, 2 = deleted/tombstone.
typedef struct {
    char *key;
    size_t key_len;
    uint64_t hash;
    int64_t value;
    uint8_t state;
} MapEntry;
typedef struct { Kind kind; uint32_t gen, next; uint64_t owner; unsigned char *data; size_t len, cap, budget; } Slot;
static Slot *slots = NULL;
static uint32_t count = 0, allocated = 0, first_free = 0;

static void nova_native_trace_if_enabled(void) {
#if defined(__linux__)
    // Native symbol addresses, NOT Nova source-level stack frames.
    const char *backtrace_setting=getenv("NOVA_BACKTRACE");
    if (backtrace_setting && backtrace_setting[0] && strcmp(backtrace_setting,"0")!=0) {
        void *frames[48];
        int n=backtrace(frames,48);
        fputs("  = native stack trace (runtime frames):\n",stderr);
        backtrace_symbols_fd(frames,n,STDERR_FILENO);
    }
#endif
}
static void die(const char *why) {
    fprintf(stderr, "nova runtime error: %s\n", why);
    nova_native_trace_if_enabled();
    fflush(stderr);
    exit(70);
}
/* Explicit panic policy: terminate process, do NOT unwind Nova RAII scopes.
 * Recoverable errors should be represented using std::result::Result<T,E>. */
void __nova_std__panic____fail(const char *message) {
    fprintf(stderr,"panic[NOVA-P001]: %s\n",message?message:"panic");
    nova_native_trace_if_enabled();
    fflush(stderr);
    exit(101);
}
static void *xmalloc(size_t size) {
    void *p = malloc(size ? size : 1);
    if (!p) die("out of memory");
    return p;
}
static void *xrealloc(void *p, size_t size) {
    void *result = realloc(p, size ? size : 1);
    if (!result) die("out of memory");
    return result;
}
static size_t checked_size(uint64_t n) {
    if (n > (uint64_t)SIZE_MAX) die("allocation size overflow");
    return (size_t)n;
}
static size_t grow(size_t cap, size_t required) {
    if (required == 0) return cap;
    size_t next = cap ? cap : 16;
    while (next < required) {
        if (next > SIZE_MAX/2) { next = required; break; }
        next *= 2;
    }
    return next;
}
static uint64_t handle_of(uint32_t index) {
    return ((uint64_t)slots[index].gen << 32) | (uint64_t)(index+1);
}
static Slot *get(uint64_t handle, Kind kind) {
    uint32_t ix = (uint32_t)handle;
    if (ix == 0 || ix > count) die("invalid handle");
    Slot *s = &slots[ix-1];
    if (s->kind == FREE || s->gen != (uint32_t)(handle >> 32)) die("use after free or stale handle");
    if (kind != FREE && s->kind != kind) die("resource has incorrect type");
    return s;
}
static uint64_t create(Kind kind) {
    uint32_t ix;
    if (first_free) {
        ix = first_free - 1;
        first_free = slots[ix].next;
    } else {
        if (count == UINT32_MAX) die("handle table exhausted");
        if (count == allocated) {
            uint32_t n = allocated ? allocated*2 : 32;
            if (n <= allocated) die("handle table overflow");
            slots = xrealloc(slots, (size_t)n*sizeof(Slot));
            memset(slots+allocated, 0, (size_t)(n-allocated)*sizeof(Slot));
            allocated = n;
        }
        ix = count++;
    }
    uint32_t gen = slots[ix].gen;
    memset(&slots[ix], 0, sizeof(Slot));
    slots[ix].gen = gen ? gen : 1;
    slots[ix].kind = kind;
    return handle_of(ix);
}
static void release_slot(uint64_t id) {
    Slot *s = get(id, FREE);
    if (s->kind == MAP_I64 || s->kind == MAP_NUMERIC) {
        MapEntry *entries = (MapEntry*)s->data;
        for (size_t j = 0; j < s->cap; ++j)
            if (entries[j].state == 1) free(entries[j].key);
    }
    free(s->data);
    s->data = NULL;
    s->kind = FREE;
    size_t used=s->len;
    s->len = s->cap = 0;
    uint64_t owner=s->owner;
    s->owner = 0;
    if (owner) {
        Slot *a=get(owner,ARENA);
        if (a->len < used) die("arena usage underflow");
        a->len -= used;
    }
    s->gen++;
    if (!s->gen) s->gen = 1;
    uint32_t ix = (uint32_t)id - 1;
    s->next = first_free;
    first_free = ix+1;
}
static void ensure_capacity(Slot *s, size_t required, bool trailing_nul, size_t element) {
    if (!element) die("zero element size");
    size_t needed = required;
    if (trailing_nul && needed == SIZE_MAX) die("string capacity overflow");
    if (trailing_nul) needed++;
    if (needed <= s->cap) return;
    size_t target = grow(s->cap, needed);
    if (target > SIZE_MAX/element) die("allocation overflow");
    s->data = xrealloc(s->data, target*element);
    s->cap = target;
}
static size_t add_size(size_t a, size_t b) {
    if (a > SIZE_MAX-b) die("length overflow");
    return a+b;
}
/* Reject invalid UTF-8, including overlong sequences, surrogates and codepoints > U+10ffff. */
static bool utf8(const unsigned char *p, size_t n, size_t *chars) {
    size_t i=0, codepoints=0;
    while (i<n) {
        uint32_t cp, min;
        unsigned char b=p[i++];
        size_t more;
        if (b<0x80) {codepoints++;continue;}
        if (b>=0xC2 && b<=0xDF) {cp=b&0x1F;more=1;min=0x80;}
        else if (b>=0xE0 && b<=0xEF) {cp=b&0x0F;more=2;min=0x800;}
        else if (b>=0xF0 && b<=0xF4) {cp=b&7;more=3;min=0x10000;}
        else return false;
        if (more>n-i) return false;
        for (size_t j=0;j<more;j++) {
            unsigned char next=p[i++];
            if ((next&0xC0)!=0x80) return false;
            cp=(cp<<6)|(next&0x3F);
        }
        if (cp<min || cp>0x10FFFF || (cp>=0xD800 && cp<=0xDFFF)) return false;
        codepoints++;
    }
    if (chars) *chars=codepoints;
    return true;
}
static void append_raw(Slot *s,const char *data,size_t n) {
    size_t newlen=add_size(s->len,n);
    ensure_capacity(s,newlen,true,1);
    if (n) memcpy(s->data+s->len,data,n);
    s->len=newlen;
    s->data[s->len]=0;
}
static uint64_t make_text(const char *src, Kind kind, size_t reserve) {
    if (!src) die("null string pointer");
    size_t n=strlen(src);
    if (!utf8((const unsigned char*)src,n,NULL)) die("invalid UTF-8");
    uint64_t h=create(kind);
    Slot *s=get(h,kind);
    ensure_capacity(s,add_size(n,reserve),true,1);
    if(n) memcpy(s->data,src,n);
    s->len=n;s->data[n]=0;
    return h;
}

/* String: opaque owned mutable UTF-8 byte buffer, explicit drop. */
uint64_t __nova_std__string____string_create_from_text(const char *text) { return make_text(text, OWN_STRING, 0); }
uint64_t __nova_std__string____string_create_empty(void) { return make_text("", OWN_STRING, 0); }
uint64_t __nova_std__string____string_clone(uint64_t id) {
    Slot *s=get(id,OWN_STRING);
    return make_text((const char*)s->data,OWN_STRING,0);
}
void __nova_std__string____string_destroy(uint64_t id) { get(id,OWN_STRING); release_slot(id); }
uint64_t __nova_std__string____len(uint64_t id) { return get(id,OWN_STRING)->len; }
uint64_t __nova_std__string____capacity(uint64_t id) { Slot*s=get(id,OWN_STRING);return s->cap ? s->cap-1 : 0; }
uint64_t __nova_std__string____chars(uint64_t id) {
    Slot*s=get(id,OWN_STRING);size_t n=0;
    if(!utf8(s->data,s->len,&n)) die("internal invalid UTF-8");
    return n;
}
void __nova_std__string____reserve(uint64_t id,uint64_t capacity) {
    Slot*s=get(id,OWN_STRING);ensure_capacity(s,checked_size(capacity),true,1);
}
void __nova_std__string____push(uint64_t id,const char *text) {
    if(!text)die("null string pointer");
    size_t n=strlen(text);
    if(!utf8((const unsigned char*)text,n,NULL))die("invalid UTF-8");
    append_raw(get(id,OWN_STRING),text,n);
}
void __nova_std__string____concat(uint64_t dest,uint64_t src) {
    Slot *a=get(dest,OWN_STRING),*b=get(src,OWN_STRING);
    if(a==b) {size_t n=b->len;char*copy=xmalloc(n+1);memcpy(copy,b->data,n+1);append_raw(a,copy,n);free(copy);return;}
    append_raw(a,(const char*)b->data,b->len);
}
void __nova_std__string____clear(uint64_t id) {
    Slot*s=get(id,OWN_STRING);s->len=0;if(s->data)s->data[0]=0;
}
bool __nova_std__string____equals(uint64_t id,const char*text) {
    if(!text)die("null string pointer");
    Slot*s=get(id,OWN_STRING);size_t n=strlen(text);
    return n==s->len && !memcmp(s->data,text,n);
}
bool __nova_std__string____contains(uint64_t id,const char*text) {
    if(!text)die("null string pointer");
    Slot*s=get(id,OWN_STRING);return strstr((char*)s->data,text)!=NULL;
}
uint64_t __nova_std__string____substring(uint64_t id,uint64_t from,uint64_t length) {
    Slot*s=get(id,OWN_STRING);size_t begin=checked_size(from),n=checked_size(length);
    if(begin>s->len || n>s->len-begin)die("substring out of bounds");
    if((begin && ((s->data[begin]&0xC0)==0x80)) ||
       (begin+n < s->len && ((s->data[begin+n]&0xC0)==0x80)))die("substring splits UTF-8 codepoint");
    char *copy=xmalloc(n+1);memcpy(copy,s->data+begin,n);copy[n]=0;
    uint64_t out=make_text(copy,OWN_STRING,0);free(copy);return out;
}
/* Bootstrap parser byte access. All offsets are UTF-8 byte offsets. */
int32_t __nova_std__string____byte_at(uint64_t id, int64_t offset) {
    Slot *s=get(id,OWN_STRING);
    if (offset<0 || (uint64_t)offset>=s->len) return -1;
    return (int32_t)s->data[offset];
}
bool __nova_std__string____span_equals(uint64_t id,int64_t start,int64_t length,const char*value) {
    Slot *s=get(id,OWN_STRING);
    if (!value || start<0 || length<0 || (uint64_t)start>s->len || (uint64_t)length>s->len-(uint64_t)start) return false;
    size_t n=strlen(value);
    return n==(size_t)length && memcmp(s->data+start,value,n)==0;
}
// Safe length of a borrowed, NUL-terminated str (distinct from String handle).
uint64_t __nova_std__string____text_length(const char *text) {
    if (!text) die("null borrowed string");
    return (uint64_t)strlen(text);
}
int64_t __nova_std__string____byte_len(uint64_t id) {
    Slot *s=get(id,OWN_STRING);
    if (s->len>INT64_MAX) die("source file too large");
    return (int64_t)s->len;
}
void __nova_std__string____print_span(uint64_t id,int64_t start,int64_t length) {
    Slot *s=get(id,OWN_STRING);
    if(start<0 || length<0 || (uint64_t)start>s->len ||
       (uint64_t)length>s->len-(uint64_t)start) die("print_span out of bounds");
    if(length) fwrite(s->data+start,1,(size_t)length,stdout);
}
void __nova_std__string____print(uint64_t id) {Slot*s=get(id,OWN_STRING);if(s->len)fwrite(s->data,1,s->len,stdout);}
void __nova_std__string____println(uint64_t id) {__nova_std__string____print(id);fputc('\n',stdout);}
void __nova_std__string____append_i64(uint64_t id,int64_t value) {
    char number[32];int n=snprintf(number,sizeof(number),"%" PRId64,value);
    if(n<0 || (size_t)n>=sizeof(number))die("number formatting failed");
    append_raw(get(id,OWN_STRING),number,(size_t)n);
}

/* StringBuilder: same growable allocation, move on finish without copying. */
uint64_t __nova_std__builder____create(void) {return make_text("",BUILDER,0);}
uint64_t __nova_std__builder____with_capacity(uint64_t cap) {
    uint64_t h=make_text("",BUILDER,0);
    ensure_capacity(get(h,BUILDER),checked_size(cap),true,1);
    return h;
}
void __nova_std__builder____append(uint64_t id,const char*text) {
    if(!text)die("null text");size_t n=strlen(text);
    if(!utf8((const unsigned char*)text,n,NULL))die("invalid UTF-8");
    append_raw(get(id,BUILDER),text,n);
}
void __nova_std__builder____append_i64(uint64_t id,int64_t value) {
    char buf[32];int n=snprintf(buf,sizeof(buf),"%" PRId64,value);
    if(n<0 || (size_t)n>=sizeof(buf))die("number formatting failed");
    append_raw(get(id,BUILDER),buf,(size_t)n);
}
void __nova_std__builder____append_bool(uint64_t id,bool value) {append_raw(get(id,BUILDER),value?"true":"false",value?4:5);}
uint64_t __nova_std__builder____len(uint64_t id) {return get(id,BUILDER)->len;}
uint64_t __nova_std__builder____finish(uint64_t id) {get(id,BUILDER)->kind=OWN_STRING;return id;}
/* Snapshot a builder without consuming it. Returns a managed String handle. */
uint64_t __nova_std__builder____snapshot(uint64_t id) {
    Slot *s=get(id,BUILDER);
    return make_text((const char*)s->data,OWN_STRING,0);
}
/* Borrowed UTF-8 view. Invalid after mutation or release of the String. */
const char *__nova_std__string____string_borrow_text(uint64_t id) {
    Slot *s=get(id,OWN_STRING);
    return (const char*)s->data;
}
void __nova_std__builder____drop(uint64_t id) {get(id,BUILDER);release_slot(id);}

/* Checked bytes: no public pointer can become dangling by resizing the allocation. */
uint64_t __nova_std__memory____buffer_new(uint64_t size) {
    size_t n=checked_size(size);uint64_t id=create(BUFFER);Slot*s=get(id,BUFFER);
    ensure_capacity(s,n,false,1);if(n)memset(s->data,0,n);s->len=n;return id;
}
void __nova_std__memory____buffer_drop(uint64_t id) {get(id,BUFFER);release_slot(id);}
uint64_t __nova_std__memory____buffer_len(uint64_t id) {return get(id,BUFFER)->len;}
uint64_t __nova_std__memory____buffer_capacity(uint64_t id) {return get(id,BUFFER)->cap;}
void __nova_std__memory____buffer_resize(uint64_t id,uint64_t size) {
    Slot*s=get(id,BUFFER);if(s->owner)die("arena buffers cannot be resized");
    size_t n=checked_size(size);ensure_capacity(s,n,false,1);
    if(n>s->len)memset(s->data+s->len,0,n-s->len);s->len=n;
}
uint8_t __nova_std__memory____buffer_read(uint64_t id,uint64_t index) {
    Slot*s=get(id,BUFFER);if(index>=s->len)die("buffer read out of bounds");return s->data[(size_t)index];
}
void __nova_std__memory____buffer_write(uint64_t id,uint64_t index,uint8_t value) {
    Slot*s=get(id,BUFFER);if(index>=s->len)die("buffer write out of bounds");s->data[(size_t)index]=value;
}
void __nova_std__memory____buffer_fill(uint64_t id,uint8_t value) {
    Slot*s=get(id,BUFFER);if(s->len)memset(s->data,value,s->len);
}
void __nova_std__memory____buffer_copy(uint64_t dst,uint64_t dst_offset,uint64_t src,uint64_t src_offset,uint64_t count_bytes) {
    Slot*d=get(dst,BUFFER),*s=get(src,BUFFER);
    if(dst_offset>d->len || src_offset>s->len || count_bytes>d->len-dst_offset || count_bytes>s->len-src_offset)
        die("buffer copy out of bounds");
    if (!count_bytes) return;
    memmove(d->data+(size_t)dst_offset,s->data+(size_t)src_offset,(size_t)count_bytes);
}

/* An arena groups owned buffers under a budget. Reset invalidates child handles. */
uint64_t __nova_std__arena____create(uint64_t capacity) {
    uint64_t h=create(ARENA);get(h,ARENA)->budget=checked_size(capacity);return h;
}
uint64_t __nova_std__arena____alloc(uint64_t arena,uint64_t size) {
    Slot*a=get(arena,ARENA);size_t n=checked_size(size);
    if(n>a->budget-a->len)die("arena capacity exceeded");
    uint64_t child=__nova_std__memory____buffer_new(size);
    a=get(arena,ARENA);a->len=add_size(a->len,n);get(child,BUFFER)->owner=arena;
    return child;
}
uint64_t __nova_std__arena____used(uint64_t id) {return get(id,ARENA)->len;}
uint64_t __nova_std__arena____capacity(uint64_t id) {return get(id,ARENA)->budget;}
void __nova_std__arena____reset(uint64_t id) {
    get(id,ARENA);
    for(uint32_t i=0;i<count;i++)if(slots[i].kind==BUFFER&&slots[i].owner==id)
        release_slot(handle_of(i));
    get(id,ARENA)->len=0;
}
void __nova_std__arena____drop(uint64_t id) {__nova_std__arena____reset(id);release_slot(id);}

/* Generic vector storage, specialized ABI for u8 and i64 until Nova has sizeof(T). */
static uint64_t vector_new(Kind kind) {return create(kind);}
static void vector_reserve(uint64_t id,Kind kind,size_t requested,size_t element) {
    Slot*s=get(id,kind);ensure_capacity(s,requested,false,element);
}
static void vector_push(uint64_t id,Kind kind,const void *item,size_t elem) {
    Slot*s=get(id,kind);size_t next=add_size(s->len,1);
    ensure_capacity(s,next,false,elem);memcpy(s->data+s->len*elem,item,elem);s->len=next;
}
uint64_t __nova_std__vec____int64_vector_create(void) {return vector_new(VECTOR_I64);}
void __nova_std__vec____int64_vector_append(uint64_t id,int64_t value) {vector_push(id,VECTOR_I64,&value,sizeof(value));}
uint64_t __nova_std__vec____int64_vector_length(uint64_t id) {return get(id,VECTOR_I64)->len;}
void __nova_std__vec____int64_vector_reserve(uint64_t id,uint64_t capacity) {vector_reserve(id,VECTOR_I64,checked_size(capacity),8);}
int64_t __nova_std__vec____int64_vector_get(uint64_t id,uint64_t idx) {
    Slot*s=get(id,VECTOR_I64);if(idx>=s->len)die("vector index out of bounds");
    int64_t v;memcpy(&v,s->data+idx*8,8);return v;
}
void __nova_std__vec____int64_vector_set(uint64_t id,uint64_t idx,int64_t value) {
    Slot*s=get(id,VECTOR_I64);if(idx>=s->len)die("vector index out of bounds");
    memcpy(s->data+idx*8,&value,8);
}
int64_t __nova_std__vec____int64_vector_pop(uint64_t id) {
    Slot*s=get(id,VECTOR_I64);if(!s->len)die("pop from empty vector");
    int64_t v;memcpy(&v,s->data+(--s->len)*8,8);return v;
}
int64_t __nova_std__vec____int64_vector_count(uint64_t id) {
    uint64_t length=__nova_std__vec____int64_vector_length(id);
    if (length>INT64_MAX) die("vector too large");
    return (int64_t)length;
}
int64_t __nova_std__vec____int64_vector_at(uint64_t id,int64_t index) {
    if (index<0) die("negative vector index");
    return __nova_std__vec____int64_vector_get(id,(uint64_t)index);
}
void __nova_std__vec____int64_vector_put(uint64_t id,int64_t index,int64_t value) {
    if (index<0) die("negative vector index");
    __nova_std__vec____int64_vector_set(id,(uint64_t)index,value);
}
void __nova_std__vec____int64_vector_destroy(uint64_t id) {get(id,VECTOR_I64);release_slot(id);}
uint64_t __nova_std__vec____byte_vector_create(void) {return vector_new(VECTOR_U8);}
void __nova_std__vec____byte_vector_append(uint64_t id,uint8_t value) {vector_push(id,VECTOR_U8,&value,1);}
uint64_t __nova_std__vec____byte_vector_length(uint64_t id) {return get(id,VECTOR_U8)->len;}
uint8_t __nova_std__vec____byte_vector_get(uint64_t id,uint64_t idx) {
    Slot*s=get(id,VECTOR_U8);if(idx>=s->len)die("vector index out of bounds");return s->data[(size_t)idx];
}
void __nova_std__vec____byte_vector_destroy(uint64_t id) {get(id,VECTOR_U8);release_slot(id);}
void __nova_std__vec____byte_vector_reserve(uint64_t id,uint64_t capacity) {
    vector_reserve(id,VECTOR_U8,checked_size(capacity),1);
}
uint64_t __nova_std__vec____byte_vector_capacity(uint64_t id) {
    return get(id,VECTOR_U8)->cap;
}
void __nova_std__vec____byte_vector_set(uint64_t id,uint64_t index,uint8_t value) {
    Slot *s=get(id,VECTOR_U8);
    if(index>=s->len)die("vector index out of bounds");
    s->data[(size_t)index]=value;
}
uint8_t __nova_std__vec____byte_vector_pop(uint64_t id) {
    Slot *s=get(id,VECTOR_U8);
    if(!s->len)die("pop from empty vector");
    return s->data[--s->len];
}
void __nova_std__vec____byte_vector_clear(uint64_t id) {
    get(id,VECTOR_U8)->len=0;
}
void __nova_std__vec____int64_vector_clear(uint64_t id) {
    get(id,VECTOR_I64)->len=0;
}


/* Compiler collections: owned string -> i64 hash map.
 * - Keys are copied on insert; all strings must be valid UTF-8.
 * - Lookup does not allocate. Tombstones are compacted on rehash.
 * - No borrowed pointers escape, and stale handles are rejected.
 * - Iteration order is intentionally unspecified; callers needing
 *   deterministic serialization must sort their keys themselves.
 */
static uint64_t map_hash(const char *key, size_t n) {
    uint64_t h = UINT64_C(14695981039346656037);
    for (size_t i = 0; i < n; ++i) {
        h ^= (unsigned char)key[i];
        h *= UINT64_C(1099511628211);
    }
    return h;
}
static MapEntry *map_entries(Slot *s) { return (MapEntry*)s->data; }
static size_t map_probe(Slot *s, const char *key, size_t len, uint64_t hash, bool *found) {
    if (!s->cap) { *found = false; return 0; }
    MapEntry *entries = map_entries(s);
    size_t at = (size_t)hash & (s->cap - 1);
    size_t deleted = SIZE_MAX;
    for (size_t visited = 0; visited < s->cap; ++visited) {
        MapEntry *e = &entries[at];
        if (e->state == 0) {
            *found = false;
            return deleted != SIZE_MAX ? deleted : at;
        }
        if (e->state == 2) {
            if (deleted == SIZE_MAX) deleted = at;
        } else if (e->hash == hash && e->key_len == len && memcmp(e->key, key, len) == 0) {
            *found = true;
            return at;
        }
        at = (at + 1) & (s->cap - 1);
    }
    *found = false;
    if (deleted != SIZE_MAX) return deleted;
    die("hash map is full");
    return 0;
}
static void map_rehash(Slot *s, size_t wanted) {
    if (wanted < 16) wanted = 16;
    if (wanted > SIZE_MAX / sizeof(MapEntry)) die("hash map capacity overflow");
    MapEntry *old = map_entries(s);
    size_t old_cap = s->cap;
    MapEntry *fresh = calloc(wanted, sizeof(MapEntry));
    if (!fresh) die("out of memory");
    s->data = (unsigned char*)fresh;
    s->cap = wanted;
    s->budget = 0;
    for (size_t i = 0; i < old_cap; ++i) {
        MapEntry e = old[i];
        if (e.state != 1) continue;
        bool found = false;
        size_t to = map_probe(s, e.key, e.key_len, e.hash, &found);
        if (found) die("hash map duplicate key on rehash");
        fresh[to] = e; // transfer key ownership, no copying
    }
    free(old);
}
static void map_validate_key(const char *key, size_t *n) {
    if (!key) die("null map key");
    *n = strlen(key);
    if (!utf8((const unsigned char*)key, *n, NULL)) die("invalid UTF-8 map key");
}
uint64_t __nova_std__map____string_int64_map_create(void) { return create(MAP_I64); }
uint64_t __nova_std__map____string_int64_map_length(uint64_t id) { return get(id, MAP_I64)->len; }
bool __nova_std__map____string_int64_map_insert(uint64_t id, const char *key, int64_t value) {
    Slot *s = get(id, MAP_I64);
    size_t n;
    map_validate_key(key, &n);
    if (s->cap == 0) map_rehash(s, 16);
    if (s->len + s->budget + 1 >= s->cap - s->cap / 4) {
        // At <=75% effective occupancy, short linear-probe chains are kept.
        // Clean tombstones before doubling when possible.
        size_t target = (s->len + 1 >= s->cap / 2) ? s->cap * 2 : s->cap;
        if (target < s->cap) die("hash map capacity overflow");
        map_rehash(s, target);
    }
    uint64_t hash = map_hash(key, n);
    bool found;
    size_t at = map_probe(s, key, n, hash, &found);
    MapEntry *e = &map_entries(s)[at];
    if (found) { e->value = value; return false; }
    if (e->state == 2) { if (!s->budget) die("hash map tombstone underflow"); s->budget--; }
    char *owned = xmalloc(add_size(n, 1));
    memcpy(owned, key, n + 1);
    *e = (MapEntry){.key = owned, .key_len = n, .hash = hash, .value = value, .state = 1};
    s->len++;
    return true;
}
bool __nova_std__map____string_int64_map_contains(uint64_t id, const char *key) {
    Slot *s = get(id, MAP_I64);
    size_t n;
    map_validate_key(key, &n);
    if (!s->cap) return false;
    bool found;
    (void)map_probe(s, key, n, map_hash(key,n), &found);
    return found;
}
int64_t __nova_std__map____string_int64_map_get_or(uint64_t id, const char *key, int64_t fallback) {
    Slot *s = get(id, MAP_I64);
    size_t n;
    map_validate_key(key, &n);
    if (!s->cap) return fallback;
    bool found;
    size_t at = map_probe(s, key, n, map_hash(key,n), &found);
    return found ? map_entries(s)[at].value : fallback;
}
bool __nova_std__map____string_int64_map_remove(uint64_t id, const char *key) {
    Slot *s = get(id, MAP_I64);
    size_t n;
    map_validate_key(key, &n);
    if (!s->cap) return false;
    bool found;
    size_t at = map_probe(s, key, n, map_hash(key,n), &found);
    if (!found) return false;
    MapEntry *e = &map_entries(s)[at];
    free(e->key);
    *e = (MapEntry){.state=2};
    s->len--;
    s->budget++;
    return true;
}
uint64_t __nova_std__map____string_int64_map_owned_key_at(uint64_t id, uint64_t position) {
    Slot *s = get(id, MAP_I64);
    if (position >= s->len) die("hash map key index out of bounds");
    MapEntry *entries = map_entries(s);
    size_t seen = 0;
    for (size_t i = 0; i < s->cap; ++i) {
        if (entries[i].state != 1) continue;
        if (seen++ == position) return __nova_std__string____string_create_from_text(entries[i].key);
    }
    die("hash map corrupted");
    return 0;
}
void __nova_std__map____string_int64_map_destroy(uint64_t id) { get(id, MAP_I64); release_slot(id); }


/* Private ABI used by std::map::Map<K,V>, with integral (one-word) keys
 * and values only. The full 64-bit key is compared byte-for-byte; keys with
 * the same low bits are never confused. The handle generation guards UAF.
 * Reuse the standard map's open-addressed/tombstone storage and growth rules.
 */
static void numeric_map_key(int64_t key, char out[8]) {
    memcpy(out, &key, sizeof(key));
}
uint64_t __nova_std__internal__map____int64_map_create(void) {
    return create(MAP_NUMERIC);
}
uint64_t __nova_std__internal__map____int64_map_length(uint64_t id) {
    return get(id,MAP_NUMERIC)->len;
}
bool __nova_std__internal__map____int64_map_insert(uint64_t id,int64_t key,int64_t value) {
    Slot *s=get(id,MAP_NUMERIC);
    char bytes[8]; numeric_map_key(key,bytes);
    if (s->cap == 0) map_rehash(s,16);
    if (s->len+s->budget+1 >= s->cap-s->cap/4) {
        size_t target=(s->len+1>=s->cap/2)?s->cap*2:s->cap;
        if (target<s->cap) die("numeric map capacity overflow");
        map_rehash(s,target);
    }
    uint64_t hash=map_hash(bytes,sizeof(bytes));
    bool found=false;
    size_t at=map_probe(s,bytes,sizeof(bytes),hash,&found);
    MapEntry *entry=&map_entries(s)[at];
    if(found) {entry->value=value;return false;}
    if(entry->state==2) {if(!s->budget)die("numeric map tombstone underflow"); s->budget--;}
    char *owned=xmalloc(sizeof(bytes)); memcpy(owned,bytes,sizeof(bytes));
    *entry=(MapEntry){.key=owned,.key_len=sizeof(bytes),.hash=hash,.value=value,.state=1};
    s->len++;
    return true;
}
bool __nova_std__internal__map____int64_map_contains(uint64_t id,int64_t key) {
    Slot*s=get(id,MAP_NUMERIC);
    if(!s->cap)return false;
    char bytes[8]; numeric_map_key(key,bytes);
    bool found=false;
    (void)map_probe(s,bytes,sizeof(bytes),map_hash(bytes,sizeof(bytes)),&found);
    return found;
}
int64_t __nova_std__internal__map____int64_map_get_or(uint64_t id,int64_t key,int64_t fallback) {
    Slot*s=get(id,MAP_NUMERIC);
    if(!s->cap)return fallback;
    char bytes[8]; numeric_map_key(key,bytes);
    bool found=false;
    size_t at=map_probe(s,bytes,sizeof(bytes),map_hash(bytes,sizeof(bytes)),&found);
    return found?map_entries(s)[at].value:fallback;
}
bool __nova_std__internal__map____int64_map_remove(uint64_t id,int64_t key) {
    Slot*s=get(id,MAP_NUMERIC);
    if(!s->cap)return false;
    char bytes[8]; numeric_map_key(key,bytes);
    bool found=false;
    size_t at=map_probe(s,bytes,sizeof(bytes),map_hash(bytes,sizeof(bytes)),&found);
    if(!found)return false;
    MapEntry*e=&map_entries(s)[at];free(e->key);*e=(MapEntry){.state=2};
    s->len--;s->budget++;return true;
}
// Iteration order follows the hash table's slots and is NOT stable across
// insertion, removal or rehash. Cursors must never outlive their map owner.
static const MapEntry *numeric_entry_at(uint64_t id, uint64_t position) {
    Slot *s=get(id,MAP_NUMERIC);
    if(position>=s->len)die("numeric map cursor out of bounds");
    MapEntry*entries=map_entries(s);
    uint64_t seen=0;
    for(size_t i=0;i<s->cap;++i) {
        if(entries[i].state!=1)continue;
        if(seen++==position)return &entries[i];
    }
    die("corrupted numeric map cursor");
    return NULL;
}
int64_t __nova_std__internal__map____int64_map_key_at(uint64_t id, uint64_t position) {
    const MapEntry *e=numeric_entry_at(id,position);
    int64_t key=0;memcpy(&key,e->key,sizeof(key));return key;
}
int64_t __nova_std__internal__map____int64_map_value_at(uint64_t id,uint64_t position) {
    return numeric_entry_at(id,position)->value;
}
void __nova_std__internal__map____int64_map_clear(uint64_t id) {
    Slot*s=get(id,MAP_NUMERIC);
    MapEntry *entries=map_entries(s);
    for(size_t i=0;i<s->cap;++i)if(entries[i].state==1)free(entries[i].key);
    if(s->cap)memset(s->data,0,s->cap*sizeof(MapEntry));
    s->len=0;s->budget=0;
}
void __nova_std__internal__map____int64_map_destroy(uint64_t id) {
    get(id,MAP_NUMERIC);release_slot(id);
}

/* Miscellaneous std utilities. */
// Blocking POSIX write; retries EINTR and returns total bytes or -1 on error.
// `str` is NUL-terminated in Nova 0.8; embedded zero bytes are not supported.
int64_t __nova_std__io____write_fd(int32_t fd, const char *text) {
    if (fd < 0 || !text) return -1;
    size_t n = strlen(text), written = 0;
    if (n > INT64_MAX) return -1;
    while (written < n) {
        size_t chunk = n - written;
        if (chunk > (size_t)(1024 * 1024)) chunk = 1024 * 1024;
        ssize_t amount = write(fd, text + written, chunk);
        if (amount < 0) {
            if (errno == EINTR) continue;
            return written ? (int64_t)written : -1;
        }
        if (amount == 0) break;
        written += (size_t)amount;
    }
    return (int64_t)written;
}
void __nova_std__io____print(const char *text) {if(!text)die("null text");fputs(text,stdout);}
void __nova_std__io____println(const char *text) {__nova_std__io____print(text);fputc('\n',stdout);}
void __nova_std__io____print_i64(int64_t value) {printf("%" PRId64,value);}
void __nova_std__io____println_i64(int64_t value) {printf("%" PRId64 "\n",value);}

/* Typed format output for built-in AST print macros (never a caller-supplied
 * printf format, never varargs). All operations write one argument once. */
void __nova_std__io____eprint(const char *text) {if(!text)die("null text");fputs(text,stderr);}
void __nova_std__io____print_bool(bool value) {fputs(value?"true":"false",stdout);}
void __nova_std__io____eprint_bool(bool value) {fputs(value?"true":"false",stderr);}
void __nova_std__io____eprint_i64(int64_t value) {fprintf(stderr,"%" PRId64,value);}
void __nova_std__io____print_u64(uint64_t value) {printf("%" PRIu64,value);}
void __nova_std__io____eprint_u64(uint64_t value) {fprintf(stderr,"%" PRIu64,value);}
static void nova_format_f64(FILE *out,double value,int32_t precision) {
    if (precision < -1 || precision > 18)die("format precision must be -1 or 0..18");
    if (precision == -1)fprintf(out,"%.15g",value);
    else fprintf(out,"%.*f",precision,value);
}
void __nova_std__io____print_f64(double value,int32_t precision) {nova_format_f64(stdout,value,precision);}
void __nova_std__io____eprint_f64(double value,int32_t precision) {nova_format_f64(stderr,value,precision);}
static void nova_format_debug_str(FILE *out,const char *value) {
    if(!value)die("null text");
    fputc('"',out);
    for(const unsigned char *p=(const unsigned char *)value; *p; ++p) {
        if(*p=='"'||*p=='\\') {fputc('\\',out);fputc(*p,out);}
        else if(*p=='\n')fputs("\\n",out);
        else if(*p=='\r')fputs("\\r",out);
        else if(*p=='\t')fputs("\\t",out);
        else fputc(*p,out);
    }
    fputc('"',out);
}
void __nova_std__io____print_debug_str(const char *value) {nova_format_debug_str(stdout,value);}
void __nova_std__io____eprint_debug_str(const char *value) {nova_format_debug_str(stderr,value);}

/* String formatter: caller owns the String handle. Chainable functions append
 * directly to the same allocation, with no intermediate owned Strings. */
uint64_t __nova_std__string____format_append_text(uint64_t handle,const char *text) {
    __nova_std__string____push(handle,text);
    return handle;
}
uint64_t __nova_std__string____format_append_bool(uint64_t handle,bool value) {
    append_raw(get(handle,OWN_STRING),value?"true":"false",value?4:5);
    return handle;
}
uint64_t __nova_std__string____format_append_i64(uint64_t handle,int64_t value) {
    __nova_std__string____append_i64(handle,value);
    return handle;
}
uint64_t __nova_std__string____format_append_u64(uint64_t handle,uint64_t value) {
    char number[32];int n=snprintf(number,sizeof(number),"%" PRIu64,value);
    if(n<0 || (size_t)n>=sizeof(number))die("number formatting failed");
    append_raw(get(handle,OWN_STRING),number,(size_t)n);
    return handle;
}
uint64_t __nova_std__string____format_append_f64(uint64_t handle,double value,int32_t precision) {
    if(precision < -1 || precision > 18)die("format precision must be -1 or 0..18");
    char number[512];
    int n=precision==-1 ? snprintf(number,sizeof(number),"%.15g",value)
                        : snprintf(number,sizeof(number),"%.*f",precision,value);
    if(n<0 || (size_t)n>=sizeof(number))die("number formatting failed");
    append_raw(get(handle,OWN_STRING),number,(size_t)n);
    return handle;
}
uint64_t __nova_std__string____format_append_debug_str(uint64_t handle,const char *value) {
    if(!value)die("null borrowed string");
    Slot *s=get(handle,OWN_STRING);
    append_raw(s,"\"",1);
    for(const unsigned char *p=(const unsigned char*)value;*p;++p) {
        switch(*p) {
            case '"': append_raw(s,"\\\"",2);break;
            case '\\': append_raw(s,"\\\\",2);break;
            case '\n': append_raw(s,"\\n",2);break;
            case '\r': append_raw(s,"\\r",2);break;
            case '\t': append_raw(s,"\\t",2);break;
            default: append_raw(s,(const char*)p,1);break;
        }
    }
    append_raw(s,"\"",1);
    return handle;
}

uint64_t __nova_std__convert____i64_to_string(int64_t value) {
    char buf[32];snprintf(buf,sizeof(buf),"%" PRId64,value);return __nova_std__string____string_create_from_text(buf);
}
int64_t __nova_std__convert____parse_i64_or(const char *text,int64_t fallback) {
    if(!text || !*text)return fallback;
    errno=0;char*end=NULL;intmax_t v=strtoimax(text,&end,10);
    if(errno==ERANGE || *end || end==text || v<INT64_MIN || v>INT64_MAX)return fallback;
    return (int64_t)v;
}
uint64_t __nova_std__memory____live_resources(void) {
    uint64_t n=0;for(uint32_t i=0;i<count;i++)if(slots[i].kind!=FREE)n++;return n;
}

/* Deliberately unsafe raw allocation. The caller must track layout and lifetime.
 * C11 aligned_alloc needs size rounded to an alignment multiple. */
static size_t raw_alignment(uint64_t a) {
    size_t align=checked_size(a);
    if(!align || (align & (align-1))) die("raw alignment must be power of two");
    if(align < sizeof(void*)) align=sizeof(void*);
    return align;
}
void *__nova_std__raw____allocate(uint64_t size,uint64_t alignment) {
    size_t align=raw_alignment(alignment),n=checked_size(size);
    if(n==0)n=1;
    if(n>SIZE_MAX-(align-1))die("raw allocation overflow");
    size_t rounded=(n+align-1)&~(align-1);
    void*ptr=aligned_alloc(align,rounded);
    if(!ptr)die("raw allocation failed");
    return ptr;
}
void __nova_std__raw____deallocate(void *ptr) {free(ptr);}
void *__nova_std__raw____reallocate(void *ptr,uint64_t old_size,uint64_t new_size,uint64_t alignment) {
    if(!ptr)return __nova_std__raw____allocate(new_size,alignment);
    void*next=__nova_std__raw____allocate(new_size,alignment);
    size_t a=checked_size(old_size),b=checked_size(new_size);
    memcpy(next,ptr,a<b?a:b);
    free(ptr);return next;
}

/* JSON-quoted UTF-8 string, including quotes; does not write stdout. */
static uint64_t json_quote_bytes(const char *text,size_t length) {
    if(!utf8((const unsigned char*)text,length,NULL))die("JSON input is not valid UTF-8");
    uint64_t h=create(OWN_STRING);
    Slot*s=get(h,OWN_STRING);
    append_raw(s,"\"",1);
    static const char hex[]="0123456789abcdef";
    for(size_t i=0;i<length;i++) {
        unsigned char c=(unsigned char)text[i];
        if(c=='"'||c=='\\') {char x[2]={'\\',(char)c};append_raw(s,x,2);}
        else if(c=='\n')append_raw(s,"\\n",2);
        else if(c=='\r')append_raw(s,"\\r",2);
        else if(c=='\t')append_raw(s,"\\t",2);
        else if(c<32) {
            char x[6]={'\\','u','0','0',hex[c>>4],hex[c&15]};
            append_raw(s,x,6);
        } else append_raw(s,(char*)&c,1);
    }
    append_raw(s,"\"",1);
    return h;
}
uint64_t __nova_std__json____quote(const char*text) {
    if(!text)die("null JSON string");
    return json_quote_bytes(text,strlen(text));
}
uint64_t __nova_std__json____quote_string(uint64_t id) {
    Slot*s=get(id,OWN_STRING);
    return json_quote_bytes((const char*)s->data,s->len);
}

int64_t __nova_std__math____min_i64(int64_t a,int64_t b) {return a<b?a:b;}
int64_t __nova_std__math____max_i64(int64_t a,int64_t b) {return a>b?a:b;}
int64_t __nova_std__math____clamp_i64(int64_t value,int64_t lo,int64_t hi) {
    if(lo>hi)die("clamp lower bound exceeds upper bound");
    return value<lo?lo:(value>hi?hi:value);
}

/* Protocol-neutral foundations: filesystem, environment and wall clock.
 * HTTP protocols are implemented in language packages, not the native runtime. */
#include <time.h>
#ifdef __unix__
#include <sys/types.h>
#include <sys/socket.h>
#include <sys/time.h>
#include <netinet/in.h>
#include <arpa/inet.h>
#include <unistd.h>
#if defined(__linux__)
#include <execinfo.h>
#endif
#endif

bool __nova_std__fs____exists(const char *path) {
    if (!path) die("null filesystem path");
    FILE *f=fopen(path,"rb");
    if (!f) return false;
    fclose(f);
    return true;
}
bool __nova_std__fs____write_text(const char *path, const char *text) {
    if (!path||!text) die("null filesystem argument");
    FILE *f=fopen(path,"wb");
    if (!f) return false;
    size_t n=strlen(text);
    bool ok=fwrite(text,1,n,f)==n;
    if (fclose(f)!=0) ok=false;
    return ok;
}
uint64_t __nova_std__fs____read_text(const char *path) {
    if (!path) die("null filesystem path");
    FILE *f=fopen(path,"rb");
    if (!f) die("cannot open text file");
    uint64_t h=__nova_std__string____string_create_empty();
    char buf[4096];
    size_t n;
    while ((n=fread(buf,1,sizeof(buf),f))!=0) {
        if (memchr(buf,'\0',n)) die("text file contains NUL bytes");
        append_raw(get(h,OWN_STRING),buf,n);
    }
    if (ferror(f)) die("cannot read text file");
    fclose(f);
    Slot *s=get(h,OWN_STRING);
    if (!utf8(s->data,s->len,NULL)) die("text file is not valid UTF-8");
    return h;
}
uint64_t __nova_std__env____get(const char *key) {
    if (!key) die("null environment key");
    const char *value=getenv(key);
    return __nova_std__string____string_create_from_text(value?value:"");
}
uint64_t __nova_std__time____unix_millis(void) {
    struct timespec ts;
    if (timespec_get(&ts,TIME_UTC)!=TIME_UTC) die("clock unavailable");
    if (ts.tv_sec<0) die("system clock before Unix epoch");
    return (uint64_t)ts.tv_sec*1000+(uint64_t)ts.tv_nsec/1000000;
}
uint64_t __nova_std__time____monotonic_millis(void) {
    struct timespec ts;
    if (clock_gettime(CLOCK_MONOTONIC,&ts)!=0) die("monotonic clock unavailable");
    if (ts.tv_sec<0) die("negative monotonic clock value");
    return (uint64_t)ts.tv_sec*1000+(uint64_t)ts.tv_nsec/1000000;
}

/* Nova derived JSON decoder (v0.9.5).
 * Strict RFC 8259 syntax: objects, arrays, strings, numbers, booleans, null.
 * Does not silently coerce scalar types. Max depth 64, max document 8 MiB.
 * The DOM is freed after typed decoding; decoded `str` values are retained
 * until process termination, because Nova `str` currently has no owner.
 */
#define NOVA_JSON_MAX_BYTES (8u * 1024u * 1024u)
#define NOVA_JSON_MAX_NODES 200000u
#define NOVA_JSON_MAX_DEPTH 64

typedef struct NovaJsonNode {
    unsigned kind;
    char *key;
    char *text;
    struct NovaJsonNode *children, *tail, *next;
} NovaJsonNode;
enum { NOVA_JSON_OBJECT=1, NOVA_JSON_ARRAY, NOVA_JSON_STRING, NOVA_JSON_NUMBER,
       NOVA_JSON_TRUE, NOVA_JSON_FALSE, NOVA_JSON_NULL };
typedef struct { const unsigned char *src; size_t pos,len; unsigned nodes; } NovaJsonParser;
static void __nova_json_fail(NovaJsonParser *p,const char *reason) {
    char message[256];
    if (p) snprintf(message,sizeof(message),"invalid JSON at byte %zu: %s",p->pos,reason);
    else snprintf(message,sizeof(message),"JSON conversion error: %s",reason);
    __nova_std__panic____fail(message);
}
static void __nova_json_ws(NovaJsonParser *p) {
    while(p->pos<p->len && (p->src[p->pos]==' '||p->src[p->pos]=='\n'||
        p->src[p->pos]=='\r'||p->src[p->pos]=='\t'))p->pos++;
}
static NovaJsonNode *__nova_json_node(NovaJsonParser*p,unsigned kind) {
    if (++p->nodes>NOVA_JSON_MAX_NODES)__nova_json_fail(p,"too many values");
    NovaJsonNode*n=xmalloc(sizeof(*n));
    memset(n,0,sizeof(*n));n->kind=kind;return n;
}
static void __nova_json_child(NovaJsonNode *parent,NovaJsonNode *n) {
    if (parent->tail)parent->tail->next=n;else parent->children=n;
    parent->tail=n;
}
static int __nova_json_hex(unsigned char c) {
    if(c>='0'&&c<='9')return c-'0';
    if(c>='A'&&c<='F')return c-'A'+10;
    if(c>='a'&&c<='f')return c-'a'+10;
    return -1;
}
static unsigned __nova_json_hex4(NovaJsonParser*p) {
    if(p->len-p->pos<4)__nova_json_fail(p,"incomplete Unicode escape");
    unsigned cp=0;
    for(unsigned i=0;i<4;i++) {
        int digit=__nova_json_hex(p->src[p->pos++]);
        if(digit<0)__nova_json_fail(p,"invalid Unicode escape");
        cp=(cp<<4)|(unsigned)digit;
    }
    return cp;
}
static void __nova_json_utf8(char*out,size_t *n,unsigned cp) {
    if(cp<0x80)out[(*n)++]=(char)cp;
    else if(cp<0x800) {
        out[(*n)++]=(char)(0xC0|(cp>>6));out[(*n)++]=(char)(0x80|(cp&63));
    }else if(cp<0x10000){
        out[(*n)++]=(char)(0xE0|(cp>>12));out[(*n)++]=(char)(0x80|((cp>>6)&63));out[(*n)++]=(char)(0x80|(cp&63));
    }else{
        out[(*n)++]=(char)(0xF0|(cp>>18));out[(*n)++]=(char)(0x80|((cp>>12)&63));
        out[(*n)++]=(char)(0x80|((cp>>6)&63));out[(*n)++]=(char)(0x80|(cp&63));
    }
}
static char *__nova_json_parse_string(NovaJsonParser*p) {
    if(p->pos>=p->len||p->src[p->pos++]!='"')__nova_json_fail(p,"expected string");
    // Output byte count cannot exceed source bytes, even for Unicode escapes.
    char*out=xmalloc(p->len-p->pos+1);size_t used=0;
    while(p->pos<p->len){
        unsigned char c=p->src[p->pos++];
        if(c=='"'){out[used]=0;return out;}
        if(c<0x20)__nova_json_fail(p,"unescaped control character in string");
        if(c!='\\'){
            if(c<0x80){out[used++]=(char)c;continue;}
            // Validate raw UTF-8 and reject overlongs/surrogates/out-of-range.
            unsigned cp=0,more=0;
            if(c>=0xC2&&c<=0xDF){cp=c&31;more=1;}
            else if(c>=0xE0&&c<=0xEF){cp=c&15;more=2;}
            else if(c>=0xF0&&c<=0xF4){cp=c&7;more=3;}
            else __nova_json_fail(p,"invalid UTF-8 sequence");
            if(p->len-p->pos<more)__nova_json_fail(p,"truncated UTF-8 sequence");
            for(unsigned i=0;i<more;i++){
                unsigned char next=p->src[p->pos++];
                if((next&0xC0)!=0x80)__nova_json_fail(p,"invalid UTF-8 continuation");
                cp=(cp<<6)|(next&63);
            }
            if(cp>0x10FFFF||(cp>=0xD800&&cp<=0xDFFF)||
                (more==1&&cp<0x80)||(more==2&&cp<0x800)||(more==3&&cp<0x10000))
                __nova_json_fail(p,"invalid Unicode code point");
            __nova_json_utf8(out,&used,cp);continue;
        }
        if(p->pos>=p->len)__nova_json_fail(p,"unfinished string escape");
        unsigned char esc=p->src[p->pos++];
        switch(esc){
            case '"':out[used++]='"';break;
            case '\\':out[used++]='\\';break;
            case '/':out[used++]='/';break;
            case 'b':out[used++]='\b';break;
            case 'f':out[used++]='\f';break;
            case 'n':out[used++]='\n';break;
            case 'r':out[used++]='\r';break;
            case 't':out[used++]='\t';break;
            case 'u':{
                unsigned cp=__nova_json_hex4(p);
                if(cp>=0xD800&&cp<=0xDBFF){
                    if(p->len-p->pos<6||p->src[p->pos]!='\\'||p->src[p->pos+1]!='u')
                        __nova_json_fail(p,"high surrogate requires low surrogate");
                    p->pos+=2;unsigned low=__nova_json_hex4(p);
                    if(low<0xDC00||low>0xDFFF)__nova_json_fail(p,"invalid low surrogate");
                    cp=0x10000+((cp-0xD800)<<10)+(low-0xDC00);
                } else if(cp>=0xDC00&&cp<=0xDFFF)__nova_json_fail(p,"isolated low surrogate");
                if(cp==0)__nova_json_fail(p,"NUL is not supported by Nova str");
                __nova_json_utf8(out,&used,cp);break;
            }
            default:__nova_json_fail(p,"unknown string escape");
        }
    }
    __nova_json_fail(p,"unterminated string");return NULL;
}
static NovaJsonNode *__nova_json_value(NovaJsonParser*p,unsigned depth);
static NovaJsonNode *__nova_json_object(NovaJsonParser*p,unsigned depth) {
    NovaJsonNode*n=__nova_json_node(p,NOVA_JSON_OBJECT);
    unsigned field_count=0;
    p->pos++;__nova_json_ws(p);
    if(p->pos<p->len&&p->src[p->pos]=='}'){p->pos++;return n;}
    for(;;){
        if(p->pos>=p->len||p->src[p->pos]!='"')__nova_json_fail(p,"expected object field name");
        if(++field_count>4096)__nova_json_fail(p,"object has too many fields");
        char*key=__nova_json_parse_string(p);
        for(NovaJsonNode*it=n->children;it;it=it->next)
            if(strcmp(it->key,key)==0)__nova_json_fail(p,"duplicate object field");
        __nova_json_ws(p);
        if(p->pos>=p->len||p->src[p->pos++]!=':')__nova_json_fail(p,"expected ':' after field name");
        __nova_json_ws(p);
        NovaJsonNode*value=__nova_json_value(p,depth+1);value->key=key;
        __nova_json_child(n,value);__nova_json_ws(p);
        if(p->pos>=p->len)__nova_json_fail(p,"unterminated object");
        if(p->src[p->pos]=='}'){p->pos++;return n;}
        if(p->src[p->pos++]!=',')__nova_json_fail(p,"expected ',' between object fields");
        __nova_json_ws(p);
    }
}
static NovaJsonNode *__nova_json_array(NovaJsonParser*p,unsigned depth) {
    NovaJsonNode*n=__nova_json_node(p,NOVA_JSON_ARRAY);
    p->pos++;__nova_json_ws(p);
    if(p->pos<p->len&&p->src[p->pos]==']'){p->pos++;return n;}
    for(;;){
        __nova_json_child(n,__nova_json_value(p,depth+1));__nova_json_ws(p);
        if(p->pos>=p->len)__nova_json_fail(p,"unterminated array");
        if(p->src[p->pos]==']'){p->pos++;return n;}
        if(p->src[p->pos++]!=',')__nova_json_fail(p,"expected ',' between array values");
        __nova_json_ws(p);
    }
}
static NovaJsonNode *__nova_json_number(NovaJsonParser*p) {
    size_t begin=p->pos;
    if(p->src[p->pos]=='-')p->pos++;
    if(p->pos>=p->len)__nova_json_fail(p,"unfinished number");
    if(p->src[p->pos]=='0')p->pos++;
    else if(p->src[p->pos]>='1'&&p->src[p->pos]<='9'){
        do{p->pos++;}while(p->pos<p->len&&p->src[p->pos]>='0'&&p->src[p->pos]<='9');
    }else __nova_json_fail(p,"invalid number");
    if(p->pos<p->len&&p->src[p->pos]=='.'){
        p->pos++;size_t first=p->pos;
        while(p->pos<p->len&&p->src[p->pos]>='0'&&p->src[p->pos]<='9')p->pos++;
        if(first==p->pos)__nova_json_fail(p,"missing fraction digits");
    }
    if(p->pos<p->len&&(p->src[p->pos]=='e'||p->src[p->pos]=='E')){
        p->pos++;if(p->pos<p->len&&(p->src[p->pos]=='+'||p->src[p->pos]=='-'))p->pos++;
        size_t first=p->pos;
        while(p->pos<p->len&&p->src[p->pos]>='0'&&p->src[p->pos]<='9')p->pos++;
        if(first==p->pos)__nova_json_fail(p,"missing exponent digits");
    }
    NovaJsonNode*n=__nova_json_node(p,NOVA_JSON_NUMBER);
    size_t length=p->pos-begin;n->text=xmalloc(length+1);
    memcpy(n->text,p->src+begin,length);n->text[length]=0;
    return n;
}
static NovaJsonNode *__nova_json_value(NovaJsonParser*p,unsigned depth) {
    if(depth>NOVA_JSON_MAX_DEPTH)__nova_json_fail(p,"nesting too deep");
    __nova_json_ws(p);
    if(p->pos>=p->len)__nova_json_fail(p,"expected value");
    unsigned char c=p->src[p->pos];
    if(c=='{')return __nova_json_object(p,depth);
    if(c=='[')return __nova_json_array(p,depth);
    if(c=='"') {NovaJsonNode*n=__nova_json_node(p,NOVA_JSON_STRING);n->text=__nova_json_parse_string(p);return n;}
    if(c=='-'||(c>='0'&&c<='9'))return __nova_json_number(p);
    const char*keyword=NULL;unsigned kind=0;
    if(c=='t'){keyword="true";kind=NOVA_JSON_TRUE;}
    if(c=='f'){keyword="false";kind=NOVA_JSON_FALSE;}
    if(c=='n'){keyword="null";kind=NOVA_JSON_NULL;}
    if(!keyword)__nova_json_fail(p,"invalid value");
    size_t n=strlen(keyword);
    if(p->len-p->pos<n||memcmp(p->src+p->pos,keyword,n)!=0)__nova_json_fail(p,"invalid JSON literal");
    p->pos+=n;return __nova_json_node(p,kind);
}
void *__nova_json_parse(const char*text){
    if(!text)__nova_json_fail(NULL,"null JSON input");
    size_t length=strlen(text);
    if(length>NOVA_JSON_MAX_BYTES)__nova_json_fail(NULL,"JSON document exceeds 8 MiB");
    NovaJsonParser p={(const unsigned char*)text,0,length,0};
    NovaJsonNode*root=__nova_json_value(&p,0);
    __nova_json_ws(&p);
    if(p.pos!=p.len)__nova_json_fail(&p,"trailing characters");
    return root;
}
void __nova_json_free(void*pointer){
    NovaJsonNode*n=pointer;
    if(!n)return;
    NovaJsonNode*child=n->children;
    while(child){NovaJsonNode*next=child->next;__nova_json_free(child);child=next;}
    free(n->key);free(n->text);free(n);
}
void __nova_json_require_object(void*node){
    if(!node||((NovaJsonNode*)node)->kind!=NOVA_JSON_OBJECT)
        __nova_json_fail(NULL,"expected object for struct");
}
void *__nova_json_field(void*node,const char*key){
    __nova_json_require_object(node);
    if(!key)__nova_json_fail(NULL,"null struct field name");
    for(NovaJsonNode*it=((NovaJsonNode*)node)->children;it;it=it->next)
        if(strcmp(it->key,key)==0)return it;
    char message[200];snprintf(message,sizeof(message),"required field '%s' is missing",key);
    __nova_json_fail(NULL,message);return NULL;
}
int64_t __nova_json_signed(void*node,int bits){
    NovaJsonNode*n=node;
    if(!n||n->kind!=NOVA_JSON_NUMBER||strpbrk(n->text,".eE"))
        __nova_json_fail(NULL,"expected integer JSON number");
    errno=0;char*end=NULL;int64_t v=strtoll(n->text,&end,10);
    if(errno==ERANGE||end==n->text||*end)__nova_json_fail(NULL,"signed integer overflow");
    if(bits!=8&&bits!=16&&bits!=32&&bits!=64)__nova_json_fail(NULL,"invalid integer width");
    if(bits<64){
        int64_t bound=(int64_t)1<<(bits-1);
        if(v < -bound || v>=bound)__nova_json_fail(NULL,"signed integer out of range");
    }
    return v;
}
uint64_t __nova_json_unsigned(void*node,int bits){
    NovaJsonNode*n=node;
    if(!n||n->kind!=NOVA_JSON_NUMBER||strpbrk(n->text,".eE")||n->text[0]=='-')
        __nova_json_fail(NULL,"expected unsigned integer JSON number");
    errno=0;char*end=NULL;uint64_t v=strtoull(n->text,&end,10);
    if(errno==ERANGE||end==n->text||*end)__nova_json_fail(NULL,"unsigned integer overflow");
    if(bits!=8&&bits!=16&&bits!=32&&bits!=64)__nova_json_fail(NULL,"invalid unsigned integer width");
    if(bits<64 && v>=((uint64_t)1<<bits))__nova_json_fail(NULL,"unsigned integer out of range");
    return v;
}
_Bool __nova_json_boolean(void*node){
    NovaJsonNode*n=node;
    if(!n||(n->kind!=NOVA_JSON_TRUE&&n->kind!=NOVA_JSON_FALSE))
        __nova_json_fail(NULL,"expected JSON boolean");
    return n->kind==NOVA_JSON_TRUE;
}
/* A JSON object uses one growable builder for all nested fields. */
uint64_t __nova_json_builder_new(void){return __nova_std__builder____create();}
void __nova_json_builder_append(uint64_t id,const char*text){__nova_std__builder____append(id,text);}
void __nova_json_builder_i64(uint64_t id,int64_t value){__nova_std__builder____append_i64(id,value);}
void __nova_json_builder_u64(uint64_t id,uint64_t value){
    char buf[32];int n=snprintf(buf,sizeof(buf),"%" PRIu64,value);
    if(n<0||(size_t)n>=sizeof(buf))die("JSON unsigned formatting failed");
    append_raw(get(id,BUILDER),buf,(size_t)n);
}
void __nova_json_builder_bool(uint64_t id,bool value){__nova_std__builder____append_bool(id,value);}
void __nova_json_builder_string(uint64_t id,const char*text){
    uint64_t quoted=__nova_std__json____quote(text);
    Slot*s=get(quoted,OWN_STRING);
    append_raw(get(id,BUILDER),(const char*)s->data,s->len);
    release_slot(quoted);
}
/* Retained copies model Nova `str` semantics until owned string fields exist.
 * The backing memory stays live for the process lifetime, then is released. */
typedef struct NovaJsonText {char*text;struct NovaJsonText*next;} NovaJsonText;
static NovaJsonText *__nova_json_texts=NULL;
static void __nova_json_release_texts(void){
    while(__nova_json_texts){NovaJsonText*next=__nova_json_texts->next;
        free(__nova_json_texts->text);free(__nova_json_texts);__nova_json_texts=next;}
}
static const char *__nova_json_retain_text(const char*text){
    static bool registered=false;
    if(!registered){atexit(__nova_json_release_texts);registered=true;}
    NovaJsonText*it=xmalloc(sizeof(*it));
    size_t len=strlen(text);it->text=xmalloc(len+1);
    memcpy(it->text,text,len+1);
    it->next=__nova_json_texts;__nova_json_texts=it;
    return it->text;
}
// Optional explicit release for transient JSON snapshots returned by toJson().
// Safe for unknown/double-released pointers: only retained allocations are freed.
int32_t __nova_std__json____release_text(const char *text){
    NovaJsonText **it=&__nova_json_texts;
    while(*it){
        NovaJsonText *current=*it;
        if(current->text==text){
            *it=current->next;
            free(current->text);
            free(current);
            return 0;
        }
        it=&current->next;
    }
    return -1;
}
const char *__nova_json_builder_finish(uint64_t id){
    Slot*s=get(id,BUILDER);
    const char*text=__nova_json_retain_text((const char*)s->data);
    release_slot(id);
    return text;
}
const char *__nova_json_string(void*node){
    NovaJsonNode*n=node;
    if(!n||n->kind!=NOVA_JSON_STRING)__nova_json_fail(NULL,"expected JSON string");
    return __nova_json_retain_text(n->text);
}
