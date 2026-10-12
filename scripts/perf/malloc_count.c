// LD_PRELOAD allocation counter for the TJS2 benchmark (evidence for the
// allocator proposal). Counts every malloc/calloc/realloc/free call.
#define _GNU_SOURCE
#include <dlfcn.h>
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static void *(*real_malloc)(size_t);
static void (*real_free)(void *);
static void *(*real_calloc)(size_t, size_t);
static void *(*real_realloc)(void *, size_t);
static atomic_ullong m_count, m_bytes, c_count, r_count, f_count;
static int resolving;
static char boot[1 << 16];
static size_t boot_used;

static void resolve(void) {
    if (real_malloc || resolving)
        return;
    resolving = 1;
    real_malloc = dlsym(RTLD_NEXT, "malloc");
    real_free = dlsym(RTLD_NEXT, "free");
    real_calloc = dlsym(RTLD_NEXT, "calloc");
    real_realloc = dlsym(RTLD_NEXT, "realloc");
    resolving = 0;
}
void *malloc(size_t n) {
    if (!real_malloc) {
        if (resolving || boot_used + n > sizeof(boot))
            resolve();
        else {
            void *p = &boot[boot_used];
            boot_used += (n + 15) & ~(size_t)15;
            return p;
        }
    }
    atomic_fetch_add(&m_count, 1);
    atomic_fetch_add(&m_bytes, n);
    return real_malloc(n);
}
void free(void *p) {
    if (!real_free) resolve();
    if (p >= (void *)boot && p < (void *)(boot + sizeof(boot)))
        return;
    atomic_fetch_add(&f_count, 1);
    real_free(p);
}
void *calloc(size_t a, size_t b) {
    if (!real_calloc) resolve();
    atomic_fetch_add(&c_count, 1);
    return real_calloc(a, b);
}
void *realloc(void *p, size_t n) {
    if (!real_realloc) resolve();
    atomic_fetch_add(&r_count, 1);
    return real_realloc(p, n);
}
__attribute__((destructor)) static void report(void) {
    fprintf(stderr,
            "ALLOC malloc=%llu calloc=%llu realloc=%llu free=%llu bytes=%llu\n",
            (unsigned long long)m_count, (unsigned long long)c_count,
            (unsigned long long)r_count, (unsigned long long)f_count,
            (unsigned long long)m_bytes);
}
