#pragma once
#include <stdlib.h>
#ifdef __APPLE__
extern "C" size_t malloc_size(const void*);
#define malloc_usable_size(p) malloc_size(p)
#else
extern "C" size_t malloc_usable_size(void*);
#endif
