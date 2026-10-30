// Robot arm controller firmware (Raspberry Pi Pico).
//
// J1-J3: NEMA17 steppers with planetary gearboxes on A4988 drivers (STEP/DIR, 1/8 steps, a
// shared active-low enable). J4, J5 and the gripper: hobby servos on 50 Hz PWM. Each stepper
// joint has a home switch (to ground) that is closed while the joint is past its home angle.
//
// Start-up: wait for the 24 V motor supply, enable the drivers, home J1-J3 together, then run a
// pick-and-place cycle: coordinated joint moves with trapezoidal speed profiles, all joints of a
// move starting and finishing together.
#include <math.h>
#include <stdio.h>
#include "pico/stdlib.h"
#include "hardware/adc.h"
#include "hardware/pwm.h"
#include "hardware/sync.h"

#define NJ 3
static const uint STEP[NJ] = {2, 4, 6};
static const uint DIR[NJ] = {3, 5, 7};
#define EN 8
static const uint HOME[NJ] = {10, 11, 12};
#define SERVOS 3
static const uint SERVO[SERVOS] = {13, 14, 15};
#define VSENSE 26 // ADC0: VMOT through 100k / 10k

// microsteps per output degree: 200 steps × 8 microsteps × gearbox / 360
static const float SPD[NJ] = {200 * 8 * 27 / 360.0f, 200 * 8 * 50 / 360.0f, 200 * 8 * 27 / 360.0f};
// the angle where each home switch changes over, approached from above
static const float HOME_DEG[NJ] = {-5.0f, -5.0f, -5.0f};
// joint speed (deg/s) and acceleration (deg/s²) limits
static const float VMAX[NJ] = {40.0f, 25.0f, 40.0f};
static const float AMAX[NJ] = {80.0f, 50.0f, 80.0f};

// --- step generation: a 20 kHz timer moves each joint one microstep towards its target per two
// ticks (10 kHz at most), setting DIR a tick before the STEP pulse when the direction changes
static volatile int32_t pos[NJ], want[NJ];
static volatile int dir_now[NJ];
static volatile bool pulse[NJ];
static volatile uint32_t ticks;

static bool tick(repeating_timer_t *t) {
    for (int j = 0; j < NJ; j++) {
        if (pulse[j]) {
            gpio_put(STEP[j], 0);
            pulse[j] = false;
            continue;
        }
        int32_t d = want[j] - pos[j];
        if (d == 0) continue;
        int s = d > 0 ? 1 : -1;
        if (s != dir_now[j]) {
            gpio_put(DIR[j], s > 0);
            dir_now[j] = s;
            continue;
        }
        gpio_put(STEP[j], 1);
        pulse[j] = true;
        pos[j] += s;
    }
    ticks++;
    return true;
}

static float deg(int j) { return pos[j] / SPD[j]; }

// --- servos: 1 ms to 2 ms pulses (the joint's -90° to 90°)
static void servo_us(int k, float us) { pwm_set_gpio_level(SERVO[k], (uint16_t)us); }
static void servo_deg(int k, float a) { servo_us(k, 1500.0f + a / 90.0f * 500.0f); }

static void wait_ms(uint32_t ms) {
    uint32_t end = ticks + ms * 20;
    while ((int32_t)(ticks - end) < 0) __wfi();
}

// --- a coordinated move: the slowest joint sets the time, every joint follows the same
// trapezoidal profile scaled to its travel; servos glide alongside
static float servo_now[SERVOS];

static void move_to(const float *q, const float *s) {
    float from[NJ], dq[NJ], T = 0.0f, ta = 0.0f;
    for (int j = 0; j < NJ; j++) {
        from[j] = want[j] / SPD[j];
        dq[j] = q[j] - from[j];
        // time for this joint alone with a trapezoid (or triangle)
        float d = fabsf(dq[j]), t1 = VMAX[j] / AMAX[j];
        float tj = d > VMAX[j] * t1 ? d / VMAX[j] + t1 : 2.0f * sqrtf(d / AMAX[j]);
        if (tj > T) {
            T = tj;
            ta = d > VMAX[j] * t1 ? t1 : tj / 2.0f;
        }
    }
    float s0[SERVOS];
    for (int k = 0; k < SERVOS; k++) s0[k] = servo_now[k];
    if (T < 0.3f) T = 0.3f, ta = 0.15f;
    uint32_t t0 = ticks;
    for (;;) {
        float t = (ticks - t0) / 20000.0f;
        if (t > T) t = T;
        // u: 0 to 1 along a trapezoid with accel time ta over T
        float vp = 1.0f / (T - ta), u;
        if (t < ta) u = 0.5f * vp / ta * t * t;
        else if (t < T - ta) u = vp * (t - 0.5f * ta);
        else u = 1.0f - 0.5f * vp / ta * (T - t) * (T - t);
        for (int j = 0; j < NJ; j++) want[j] = (int32_t)lroundf((from[j] + dq[j] * u) * SPD[j]);
        for (int k = 0; k < SERVOS; k++) {
            servo_now[k] = s0[k] + (s[k] - s0[k]) * u;
            servo_deg(k, servo_now[k]);
        }
        if (t >= T) break;
        wait_ms(1);
    }
    // let the steppers catch up with the last targets
    for (int j = 0; j < NJ; j++)
        while (pos[j] != want[j]) __wfi();
}

static bool home_closed(int j) { return !gpio_get(HOME[j]); }

// home J1-J3 together: run each down until its switch closes (backing up first if it already
// is), then call that the switch's angle
static void home_all(void) {
    const float slow = 10.0f; // deg/s
    bool done[NJ];
    for (int j = 0; j < NJ; j++) {
        done[j] = false;
        if (home_closed(j)) {
            printf("J%d starts on its switch, backing off\n", j + 1);
            while (home_closed(j)) {
                want[j] += (int32_t)(SPD[j] * slow / 1000.0f) + 1;
                wait_ms(1);
            }
            wait_ms(50);
        }
    }
    float acc[NJ] = {0};
    for (int n = 0; n < 20000; n++) {
        bool all = true;
        for (int j = 0; j < NJ; j++) {
            if (done[j]) continue;
            if (home_closed(j)) {
                done[j] = true;
                // the switch closed at the home angle: rebase the step count there
                uint32_t irq = save_and_disable_interrupts();
                int32_t lag = want[j] - pos[j];
                pos[j] = (int32_t)lroundf(HOME_DEG[j] * SPD[j]);
                want[j] = pos[j] + lag;
                restore_interrupts(irq);
                printf("J%d home switch at %.0f ms\n", j + 1, (double)(ticks / 20));
                continue;
            }
            all = false;
            acc[j] += SPD[j] * slow / 1000.0f;
            want[j] -= (int32_t)acc[j];
            acc[j] -= (int32_t)acc[j];
        }
        if (all) break;
        wait_ms(1);
    }
    for (int j = 0; j < NJ; j++)
        while (pos[j] != want[j]) __wfi();
}

typedef struct {
    const char *name;
    float q[NJ];      // J1, J2, J3 (deg)
    float s[SERVOS];  // J4, J5, gripper (deg)
    uint32_t dwell;   // ms to wait after it
} Pose;

#define OPEN -20.0f
#define SHUT 0.0f
// joint angles from the arm's geometry (shoulder 160 mm up, links 200 and 170 mm, wrist to finger
// pads 120 mm): the pick and place spots are 220 mm out at ±35°, the pads 30 mm above the table,
// the gripper pointing straight down; "above" is 70 mm higher
static const Pose CYCLE[] = {
    {"ready", {0, 0, 0}, {0, 0, OPEN}, 0},
    {"above pick", {35, 28.5f, 14.4f}, {47.1f, 0, OPEN}, 0},
    {"pick", {35, 45.2f, 17.4f}, {27.4f, 0, OPEN}, 100},
    {"grip", {35, 45.2f, 17.4f}, {27.4f, 0, SHUT}, 250},
    {"lift", {35, 28.5f, 14.4f}, {47.1f, 0, SHUT}, 0},
    {"above place", {-35, 28.5f, 14.4f}, {47.1f, 90, SHUT}, 0},
    {"place", {-35, 45.2f, 17.4f}, {27.4f, 90, SHUT}, 100},
    {"release", {-35, 45.2f, 17.4f}, {27.4f, 90, OPEN}, 250},
    {"ready", {0, 0, 0}, {0, 0, OPEN}, 0},
};

int main() {
    stdio_init_all();
    gpio_init(EN);
    gpio_set_dir(EN, GPIO_OUT);
    gpio_put(EN, 1); // drivers off until the supply is up
    for (int j = 0; j < NJ; j++) {
        gpio_init(STEP[j]);
        gpio_set_dir(STEP[j], GPIO_OUT);
        gpio_put(STEP[j], 0);
        gpio_init(DIR[j]);
        gpio_set_dir(DIR[j], GPIO_OUT);
        gpio_put(DIR[j], 1);
        dir_now[j] = 1;
        gpio_init(HOME[j]);
        gpio_pull_up(HOME[j]);
    }
    for (int k = 0; k < SERVOS; k++) {
        gpio_set_function(SERVO[k], GPIO_FUNC_PWM);
        uint sl = pwm_gpio_to_slice_num(SERVO[k]);
        pwm_set_clkdiv(sl, 125.0f); // 1 µs counts
        pwm_set_wrap(sl, 19999);    // 20 ms
        pwm_set_enabled(sl, true);
        servo_deg(k, 0.0f);
    }
    adc_init();
    adc_gpio_init(VSENSE);
    adc_select_input(0);
    static repeating_timer_t timer;
    add_repeating_timer_us(-50, tick, NULL, &timer);

    printf("arm controller up\n");
    float v;
    while ((v = adc_read() * 3.3f / 4095.0f * 11.0f) < 20.0f) wait_ms(5);
    printf("VMOT %.1f V, drivers on\n", (double)v);
    gpio_put(EN, 0);
    wait_ms(20);
    home_all();
    printf("homed: J1 %.1f, J2 %.1f, J3 %.1f deg\n", (double)deg(0), (double)deg(1), (double)deg(2));
    for (unsigned i = 0; i < sizeof CYCLE / sizeof CYCLE[0]; i++) {
        const Pose *p = &CYCLE[i];
        uint32_t t0 = ticks;
        move_to(p->q, p->s);
        printf("%s: J1 %.1f J2 %.1f J3 %.1f deg (%lu ms)\n", p->name, (double)deg(0), (double)deg(1), (double)deg(2), (unsigned long)((ticks - t0) / 20));
        wait_ms(p->dwell);
    }
    printf("cycle done\n");
    gpio_put(EN, 1);
    while (true) wait_ms(1000);
}
