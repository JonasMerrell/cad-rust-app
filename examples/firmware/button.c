#include "pico/stdlib.h"
int main() {
    gpio_init(15); gpio_set_dir(15, GPIO_IN); gpio_pull_up(15);
    gpio_init(16); gpio_set_dir(16, GPIO_OUT);
    while (true) { gpio_put(16, !gpio_get(15)); sleep_us(100); }
}
