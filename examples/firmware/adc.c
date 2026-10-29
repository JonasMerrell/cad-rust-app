#include <stdio.h>
#include "pico/stdlib.h"
#include "hardware/adc.h"
#include "hardware/pwm.h"
// read GP26 and set a PWM duty on GP16 proportional to it
int main() {
    stdio_init_all();
    adc_init(); adc_gpio_init(26); adc_select_input(0);
    gpio_set_function(16, GPIO_FUNC_PWM);
    uint s = pwm_gpio_to_slice_num(16);
    pwm_set_wrap(s, 4095); pwm_set_enabled(s, true);
    while (true) {
        uint16_t v = adc_read();
        pwm_set_gpio_level(16, v);
        printf("adc %u %.3f V\n", v, v * 3.3f / 4096);
        sleep_ms(10);
    }
}
