#include <stdio.h>
#include "pico/stdlib.h"
#include "pico/multicore.h"
void core1(void) {
    gpio_init(17); gpio_set_dir(17, GPIO_OUT);
    while (true) { uint32_t v = multicore_fifo_pop_blocking(); gpio_put(17, v & 1); multicore_fifo_push_blocking(v * 2); }
}
int main() {
    stdio_init_all();
    multicore_launch_core1(core1);
    for (uint32_t i = 1;; i++) { multicore_fifo_push_blocking(i); printf("core1 says %lu\n", multicore_fifo_pop_blocking()); sleep_ms(20); }
}
