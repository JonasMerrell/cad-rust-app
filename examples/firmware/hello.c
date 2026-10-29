#include <stdio.h>
#include <math.h>
#include "pico/stdlib.h"
int main() {
    stdio_init_all();
    for (int i = 0;; i++) {
        float f = sqrtf((float)i) * 1.5f; double d = sin(i * 0.1) / 3.0;
        printf("hello %d %.4f %.6f %lld\n", i, f, d, (long long)i * 1000000007LL / 3);
        sleep_ms(100);
    }
}
