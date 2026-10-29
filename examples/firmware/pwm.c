#include "pico/stdlib.h"
#include "hardware/pwm.h"
#include "hardware/clocks.h"
// 1 kHz 25% on GP0 (slice 0 A), 50% on GP1
int main() {
    gpio_set_function(0, GPIO_FUNC_PWM); gpio_set_function(1, GPIO_FUNC_PWM);
    uint s = pwm_gpio_to_slice_num(0);
    pwm_config c = pwm_get_default_config();
    pwm_config_set_clkdiv(&c, clock_get_hz(clk_sys) / 1000.0f / 1000.0f);
    pwm_config_set_wrap(&c, 999);
    pwm_init(s, &c, false);
    pwm_set_chan_level(s, PWM_CHAN_A, 250); pwm_set_chan_level(s, PWM_CHAN_B, 500);
    pwm_set_enabled(s, true);
    while (true) tight_loop_contents();
}
