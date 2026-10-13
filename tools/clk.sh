#!/bin/sh
# click at coordinates measured on a 1024-wide downscaled screenshot of the 1920x1200 display
X=$(python3 -c "print(round($1*1.875))"); Y=$(python3 -c "print(round($2*1.875))")
DISPLAY=:2 xdotool mousemove $X $Y click ${3:-1}
