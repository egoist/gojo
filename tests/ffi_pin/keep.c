// A C library that keeps the pointers it is given and writes through them
// later, as libraries with callbacks, async I/O or registered buffers do.
#include <stdint.h>

static int64_t *kept[4096];

void keep(int64_t i, int64_t *p) { kept[i] = p; }

void poke(int64_t i, int64_t v) { *kept[i] = v; }
