# FPV laser tag tank

`parts.rhai` builds a small tracked tank for FPV infrared laser tag: about 156 × 114 × 86 mm and
330 g ready to drive. Every printed part prints without supports. The turret turns ±90° and the gun elevates from −6° to +20°; the camera
elevates with it, so the barrel stays at the same place in the video and works as the aim mark. A Radxa Zero 3W streams H.264 video from a Pi camera over WiFi to a phone,
which sends the driving commands back; an Xbox style controller pairs with the phone. An RP2040-Zero
does the real-time work (motors, servos, IR, LED, battery) and talks to the Radxa over UART.

    mkdir out && rcad script examples/fpv-tank/parts.rhai out

It writes one STEP and one STL per printed part, each lying in its print orientation and centred on
the bed, `fpv-tank-assembly.step`, renders, and `parts.json`. That file lists each part's estimated
printed mass, its print size and whether it fits a 254 mm cube, and every pair of parts in the
assembly that overlap, with the gun checked at full depression, level and full elevation (there
should be none). The `stl/` folder here has a build of the STLs.

<img alt="The tank" src="fpv-tank.png" width="49%"> <img alt="X-ray view with the electronics" src="fpv-tank-xray.png" width="49%">
<img alt="From the front" src="fpv-tank-front.png" width="49%"> <img alt="Gun elevated 15°" src="fpv-tank-elevated.png" width="49%">
<img alt="Side view, gun elevated 15°" src="fpv-tank-elevated-side.png" width="100%">

## Motion check

    rcad script examples/fpv-tank/parts.rhai out motion

`motion.rhai` moves the mechanisms through their whole range and prints a JSON report:

- **Clashes:** the turret at −90°…+90° (7 steps) against the gun at −6°, 0°, 10° and 20°, with the
  elevation horn and the pushrod moving with the gun. Each pose checks the moving parts against the
  hull, deck, battery and boards, and the gun and pushrod against the turret. 28 poses, no clashes.
- **Elevation linkage:** a parallelogram (10 mm horn, 10 mm drum arm, 34 mm rod), so the gun turns
  as far as the servo does; the rod is 70°–90° to the horn over the whole range.
- **Track:** the band's pitch line (its mid-plane) is 335.9 mm round the wheels, against 42 lugs ×
  8 mm = 336 mm printed; the sprocket's pockets are 8.0 mm apart on the same line.
- **Loads:** a pivot turn on a grippy floor needs about 0.012 N·m per track, under half an N20's
  rated torque. The gun needs 1/80 and the turret 1/40 of an SG90's stall torque. Top speed is about
  0.4 m/s.

## Printing without supports

Every part is drawn for its print orientation: 45° cones and chamfers instead of flat overhangs,
horizontal posts with a 45° point underneath, and no bridge longer than 12 mm (the motor pockets).
The track lugs, wheel grooves and sprocket pockets all have 45° sides; the gun drum prints as two
halves. `check_overhangs.py` lists every downward face steeper than 45° that isn't on the bed:

    python3 examples/fpv-tank/check_overhangs.py out/*.stl

What it still lists is short bridges: hole and slot ceilings, the deck's engraving, the 1 mm hatch
ring on the cupola, and the motor pockets.

## Animation

    mkdir -p out/frames && rcad script examples/fpv-tank/parts.rhai out animate
    ffmpeg -framerate 15 -i out/frames/f%04d.png -c:v libx264 -pix_fmt yuv420p fpv-tank-drive.mp4

`anim.rhai` drives the tank around an arena: the hull's speed and turn rate come from the two track
speeds (skid steering, 89 mm between the track centres), the wheels, sprockets and grousers turn to
match, then it traverses the turret, elevates the gun and fires the IR beam at a second tank, whose
hit LED lights up. Left: a chase camera. Right: what the tank's camera sees (OV5647, 53° × 41°), with
the barrel at the bottom as the aim mark. About 150 frames, 20 minutes to render.

<img alt="The tank driving, turning, aiming and firing; chase view and the tank's camera view" src="fpv-tank-drive.gif" width="100%">

## Printed parts

| part | qty | material | notes |
|---|---|---|---|
| hull-tub | 1 | PLA or ABS | open top up; upper and lower glacis, sloped rear plate; motor pockets, wheel axle bosses, idler slots, battery strap slots, Radxa standoffs |
| deck | 1 | PLA or ABS | prints upside down (flat top on the bed); fenders and side skirts over the tracks, engraved grille and hatch, countersunk screws, SG90 hangs underneath |
| turret-ring | 1 | PLA | prints upside down; the servo horn screws into the cross recess, the arc slot lets the wires through for ±90° |
| turret-shell | 1 | PLA | prints roof down; faceted with a rear bustle, round embrasure for the gun drum with trunnion brackets either side, brackets for the elevation servo in the bustle, 4 hit sensor windows; 4 tubes from the roof take the screws that hold it to the ring |
| gun-drum-left, gun-drum-right | 1 each | PLA | two halves, each printed on its outer side face, glued together; the Pi camera screws to 4 standoffs behind the front window (mounted upside down, flip the image in software), barrel socket below it; turns on two M2 screws, open at the back for the camera ribbon |
| cupola | 1 | PLA | spigot into the turret roof; the hit LED shines through it |
| barrel | 1 | PLA | prints standing on its back end; 5 mm IR LED at the back, glued into the drum's socket |
| sprocket | 2 | PLA | D bore for the N20 shaft (glue it on), 10 pockets for the track lugs; outer face on the bed |
| idler | 2 | PLA | turns on a brass sleeve on an M3 × 35 screw in the tension slot |
| road-wheel | 8 | PLA | turn on brass sleeves on M3 × 30 screws threaded into the hull bosses |
| track | 2 | TPU 95A | a loop standing on the bed, 42 lugs at 8 mm pitch inside, grousers outside; the lugs' 45° side goes down on the bed |

About 220 g of filament per tank. Print the turret, gun drum and barrel in black, or paint them
inside: light PLA lets 940 nm light through, so the IR LED would leak into your own hit sensors.

The wheels can't turn on the screws themselves, because tightening the screw clamps the wheel. Cut
4 mm brass tube (3.1 mm bore) into 20.5 mm sleeves, one per wheel: the screw clamps the sleeve
against the hull and the wheel turns on it.

### Assembly order for the turret

1. Screw the turret servo under the deck, put the horn on, fit the deck.
2. Put the ring on the horn. Drive the horn's centre screw through the ring's middle hole, then the 4
   horn screws.
3. Build the gun into the shell: elevation servo on its brackets, horn and pushrod, the drum with
   the camera and barrel on its two M2 pivot screws (from outside).
4. Feed the camera ribbon through the ring's slot, lower the shell onto the ring, and drop the 4 M2
   × 10 countersunk screws down the tubes from the roof.

## Bought parts (per tank)

| part | approx. cost |
|---|---|
| Radxa Zero 3W, 1 GB (quad A55, WiFi, H.264 encoder) + u.FL antenna | $22 |
| OV5647 camera (Pi Camera v1.3 or clone, 25 × 24) + 22-to-15 pin Zero ribbon, 20 cm | $10 |
| microSD card, 16 GB | $5 |
| RP2040-Zero (real-time IO) | $4 |
| 2 × N20 gear motor, 6 V, ~300 rpm, 3 mm D shaft | $6 |
| 2 × SG90 micro servo (turret, gun elevation) | $4 |
| DRV8833 dual motor driver board | $1.50 |
| MP1584 buck converter set to 5 V | $1 |
| second MP1584 for the servos | $1 |
| 2S 7.4 V 450 mAh LiPo (about 58 × 31 × 14) with an XT30 or JST-PH lead | $7 |
| 940 nm 5 mm IR LED (TSAL6100), 4 × TSOP38238, WS2812 LED | $4 |
| 3 A rocker switch, 3 A fuse, AO3400 FET, 1N4148, capacitors (2 × 470 µF, 220 µF, 6 × 100 nF, 4 × 4.7 µF), resistors | $3 |
| 4 mm brass tube, 300 mm (wheel sleeves) | $2 |
| screws: M3 × 30 (8), M3 × 35 (2), M3 countersunk (4), M3 nuts (2), M1.6 × 4 (4), M2 × 6 (9), M2 × 10 countersunk (4), M2.5 × 5 (4); 1 mm steel wire for the pushrod | $3 |
| silicone wire, heat-shrink, servo leads | $1 |
| filament | $4 |
| **total** | **about $79** |

That is about $4 over the $75 target; the cheapest savings are an OV5647 clone camera and buying
the small parts in bulk for all tanks. Not counted: a 2S balance charger and a LiPo bag, shared by
the fleet.

At 300 rpm on the sprocket the tank does about 0.4 m/s (limit the PWM to about 75 % on a
full 2S pack, which is 8.4 V on a 6 V motor).

## Wiring

Battery → 3 A fuse → rocker switch → three branches:

- the DRV8833's VM (220 µF across it, and 10 µF ceramic at the pin);
- buck 1, set to 5.1 V before connecting anything and the trimmer glued: the Radxa (5 V header pin,
  220 µF there), the RP2040-Zero and the WS2812 (through a 1N4148 in its supply, so its data input
  accepts 3.3 V; 330 Ω in the data line);
- buck 2, 5 V: both servos (470 µF at their connectors) and the IR LED. Servos that stall at an end
  stop draw over 1 A; on the same buck as the Radxa they would reset it.

The camera ribbon goes from the back of the gun drum down through the turret ring's arc slot and
the slot in the deck to the Radxa, so it twists with the turret and bends with the gun: leave a
loop of slack in the turret, and keep a spare ribbon.

The Radxa runs the video, WiFi and game logic and talks to the RP2040 over UART. Use UART4 or UART5
(enable it with `rsetup`), not the UART on header pins 8 and 10, which is the boot console. RP2040-Zero
pins: GP0/GP1 UART, GP2–GP5 motor PWM at 20 kHz or more (AIN1, AIN2, BIN1, BIN2; ramp it, and keep
it to about 75 % on a full pack), GP6 turret servo, GP7 gun servo, GP8 the IR LED through an AO3400
(38 kHz carrier from a PIO), GP11–GP14 one TSOP each (so the app can show where a hit came from),
GP10 WS2812, GP26 battery voltage through a 100k/33k divider with 100 nF to ground. Power the TSOPs
from the RP2040's 3.3 V, each through 100 Ω with 4.7 µF: on 5 V their outputs would put 5 V on the
RP2040's pins. Put 100 nF across each motor and twist the motor leads. rCAD's PCB tab can simulate
the RP2040 firmware.

Warn at 3.5 V per cell and shut the Radxa down at 3.3 V per cell. Expect about 20 minutes per pack.

## IR tag notes

- The TSOP38238 is very sensitive: a 5 mm LED at full current would hit everyone in the beam's
  halo, and bounce hits off white walls. Start with about 10 mA (330 Ω from 5 V) and turn it up until
  an aimed hit works at 4 m but a wall bounce at 1 m doesn't. A 10 mm plano-convex lens at the
  muzzle (focal length 15–20 mm, LED at the focus) narrows the beam to about one tank at 3 m.
- The camera and barrel axes are parallel and 11 mm apart, so the beam lands at the centre of the
  video at any range. Draw a crosshair there in the app; don't aim with the muzzle, which shows
  about 15° low.
- Send the player and team in each shot (NEC address and command work), ignore your own shots, and
  give each gun a reload time.
- For several tanks, use a 5 GHz access point and 2–3 Mbit/s H.264 per stream.

## Still to check on real parts

- Radxa's docs list the Pi Camera v1.3 (OV5647) and v2 (IMX219) as supported on the 4-lane 22-pin
  CSI connector (enable the overlay with `rsetup`); check the ribbon's orientation, since some
  boards shipped with the connector reversed. Hardware H.264 streaming from the camera goes through
  Rockchip's MPP encoder (for example GStreamer's `mpph264enc`); try it on one board before buying
  four.
- The Zero 3W's WiFi antenna: check whether yours uses an on-board antenna or the u.FL socket; with
  the u.FL antenna, run it high under the deck, away from the battery and motor leads.
- The camera position in the drum is from the Pi Camera v1.3 drawing; check it with a camera in hand.
- Gun elevation: the servo horn and the drum's arm are both 10 mm long and point straight down at
  level, joined by a 34 mm wire pushrod (a parallelogram), so the gun turns as far as the servo does;
  limit it to −6°…+20° in the firmware.
- TPU track length: printed so its mid-plane is 336 mm, the belt length round the wheels; the idler
  slot gives ±3 mm (about ±6 mm of belt).
- Sprocket D bore: drawn for a 3 mm shaft with its flat 1.0 mm from the centre; check your motors.
- Turret horn: the ring's recess and screw holes are drawn for a standard SG90 cross horn.
- The barrel is a plain tube; a 10 mm lens at the muzzle would give a tighter, longer beam.
- Ground clearance is about 6 mm: fine on hard floors and thin rugs.
- The battery comes out with the deck off.
- The barrel fills the bottom third of the camera's view (see the animation) with the standard
  OV5647 lens. A 65°–75° OV5647 makes it smaller and is easier to drive with; much wider lenses make
  a tank at 3 m only a few pixels across.
- The barrel is the part most likely to break in a crash; a 10 mm brass tube in place of the printed
  one is stronger.
