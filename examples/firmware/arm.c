#include <stdio.h>
#include "pico/stdlib.h"
#include "hardware/adc.h"
#include "hardware/pwm.h"
// home an arm: run its motor (a MOSFET on GP15) until the limit switch (GP14 to ground) closes,
// printing the arm angle from a potentiometer on GP26; then sweep a servo on GP16 from 1 ms to 2 ms
#define MOTOR 15
#define LIMIT 14
#define SERVO 16

// the potentiometer spans the arm's 0..90 degrees
static int angle(void) {
    return adc_read() * 90 / 4095;
}

int main() {
    stdio_init_all();
    gpio_init(MOTOR); gpio_set_dir(MOTOR, GPIO_OUT); gpio_put(MOTOR, 0);
    gpio_init(LIMIT); gpio_pull_up(LIMIT);
    adc_init(); adc_gpio_init(26); adc_select_input(0);
    // 50 Hz servo pulses in microseconds
    gpio_set_function(SERVO, GPIO_FUNC_PWM);
    uint s = pwm_gpio_to_slice_num(SERVO);
    pwm_set_clkdiv(s, 125.0f);
    pwm_set_wrap(s, 19999);
    pwm_set_gpio_level(SERVO, 1000);
    pwm_set_enabled(s, true);
    printf("homing from %d deg\n", angle());
    gpio_put(MOTOR, 1);
    int ms = 0;
    while (gpio_get(LIMIT)) {
        sleep_ms(1);
        if (++ms % 50 == 0) printf("arm %d deg\n", angle());
    }
    gpio_put(MOTOR, 0);
    printf("limit switch at %d deg after %d ms\n", angle(), ms);
    for (int us = 1000; us <= 2000; us += 100) {
        pwm_set_gpio_level(SERVO, us);
        sleep_ms(40);
    }
    printf("servo at 2 ms\n");
    while (true) sleep_ms(1000);
}
