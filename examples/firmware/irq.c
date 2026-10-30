#include <stdio.h>
#include "pico/stdlib.h"
#include "hardware/sync.h"
static volatile int n;
void cb(uint gpio, uint32_t ev) { n++; gpio_put(18, n & 1); }
bool tick(repeating_timer_t *t) { gpio_xor_mask(1u << 19); return true; }
int main() {
    stdio_init_all();
    gpio_init(18); gpio_set_dir(18, GPIO_OUT); gpio_init(19); gpio_set_dir(19, GPIO_OUT);
    gpio_init(14); gpio_set_dir(14, GPIO_IN); gpio_pull_down(14);
    gpio_set_irq_enabled_with_callback(14, GPIO_IRQ_EDGE_RISE, true, cb);
    repeating_timer_t t; add_repeating_timer_us(-500, tick, NULL, &t);
    while (true) { __wfi(); }
}
