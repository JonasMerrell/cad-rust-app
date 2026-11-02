# FPV laser tag tank

`parts.rhai` builds a small tracked tank for FPV infrared laser tag: about 159 × 114 × 85 mm and
320 g ready to drive. The turret turns ±90° and the gun elevates from −6° to +20°; the camera
elevates with it, so the barrel stays at the same place in the video and works as the aim mark. A phone shows the camera feed over WiFi and sends the driving commands; an Xbox
style controller pairs with the phone.

    mkdir out && rcad script examples/fpv-tank/parts.rhai out

It writes one STEP and one STL per printed part, each lying in its print orientation and centred on
the bed, `fpv-tank-assembly.step`, renders, and `parts.json`. That file lists each part's estimated
printed mass, its print size and whether it fits a 254 mm cube, and every pair of parts in the
assembly that overlap, with the gun checked at full depression, level and full elevation (there
should be none). The `stl/` folder here has a build of the STLs.

<img alt="The tank" src="fpv-tank.png" width="49%"> <img alt="X-ray view with the electronics" src="fpv-tank-xray.png" width="49%">
<img alt="From the front" src="fpv-tank-front.png" width="49%"> <img alt="Gun elevated 15°" src="fpv-tank-elevated.png" width="49%">
<img alt="Side view, gun elevated 15°" src="fpv-tank-elevated-side.png" width="100%">

## Printed parts

| part | qty | material | notes |
|---|---|---|---|
| hull-tub | 1 | PLA or ABS | open top up; upper and lower glacis, sloped rear plate; motor pockets, wheel axle bosses, idler slots, battery strap slots |
| deck | 1 | PLA or ABS | prints upside down (flat top on the bed); fenders and side skirts over the tracks, engraved grille and hatch, countersunk screws, SG90 hangs underneath |
| turret-ring | 1 | PLA | prints upside down; the servo horn screws into the cross recess, the arc slot lets the wires through for ±90° |
| turret-shell | 1 | PLA | prints roof down; faceted with a rear bustle, round embrasure for the gun drum with trunnion brackets either side, brackets for the elevation servo in the bustle, 4 hit sensor windows |
| gun-drum | 1 | PLA | prints on its side (the top side bridges 28 mm); holds the XIAO stack with the camera at the front window and the barrel socket below it, turns on two M2 screws, open at the back for the XIAO and its wires |
| cupola | 1 | PLA | spigot into the turret roof; the hit LED shines through it |
| barrel | 1 | PLA | prints standing on its back end; 5 mm IR LED at the back, glued into the drum's socket |
| sprocket | 2 | PLA | D bore for the N20 shaft, 10 pockets for the track lugs; outer face on the bed |
| idler | 2 | PLA | on an M3 screw in the tension slot |
| road-wheel | 8 | PLA | on M3 × 30 screws threaded into the hull bosses |
| track | 2 | TPU 95A | a loop standing on the bed, 42 lugs at 8 mm pitch inside, grousers outside |

About 215 g of filament per tank.

## Bought parts (per tank)

| part | approx. cost |
|---|---|
| Seeed XIAO ESP32S3 Sense (camera, WiFi, 8 MB PSRAM) | $14 |
| 2 × N20 gear motor, 6 V, ~300 rpm, 3 mm D shaft | $6 |
| 2 × SG90 micro servo (turret, gun elevation) | $4 |
| DRV8833 dual motor driver board | $1.50 |
| MP1584 buck converter set to 5 V | $1 |
| 2S 7.4 V 450 mAh LiPo (about 58 × 31 × 14) | $7 |
| 940 nm 5 mm IR LED, 4 × TSOP38238, WS2812 LED, mini slide switch | $4 |
| M3 × 30 screws (10), M3 nuts (2), M1.6 × 4 (4), M2 × 6 (7), 1 mm steel wire for the pushrod | $2 |
| filament | $4 |
| **total** | **about $44** |

At 300 rpm on the 13.7 mm sprocket the tank does about 0.4 m/s (limit the PWM to about 75 % on a
full 2S pack, which is 8.4 V on a 6 V motor).

## Wiring

2S pack → switch → DRV8833 VM and the buck; the buck's 5 V feeds the XIAO's 5 V pin, the servo and
the LED. Suggested XIAO pins (check against the Sense board's SD card pins if you use the card):
D0–D3 motor PWM (AIN1, AIN2, BIN1, BIN2), D4 turret servo, D9 gun elevation servo, D5 IR LED through an NPN or logic FET with a
series resistor, D6 the four TSOP outputs wired together (open collector, active low), D7 WS2812,
D8 battery voltage through a 100k/33k divider.

## Still to check on real parts

- XIAO Sense camera position in the drum is from the datasheet outline; check it with a board in
  hand. The USB-C port can't be reached once the XIAO is in the drum, so flash it first and use
  WiFi (OTA) updates after that.
- Gun elevation: the elevation servo's horn and the drum's pushrod hole are both 13 mm from their
  axes, so the gun turns as far as the servo does; limit it to −6°…+20° in the firmware.
- TPU track length: printed at the exact belt length (336 mm inside), the idler slot gives ±3 mm.
- The barrel is a plain tube; a 10 mm lens at the muzzle would give a tighter, longer beam.
- Ground clearance is 5 mm: fine on hard floors and thin rugs.
