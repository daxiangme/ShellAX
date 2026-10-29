#!/bin/bash
# prints ANSI 16, 256-color cube and a truecolor gradient
for i in {0..15}; do printf "\e[48;5;${i}m  \e[0m"; done; echo
for r in {0..5}; do for g in {0..5}; do for b in {0..5}; do i=$((16+36*r+6*g+b)); printf "\e[48;5;${i}m \e[0m"; done; done; echo; done
for i in {232..255}; do printf "\e[48;5;${i}m  \e[0m"; done; echo
for i in $(seq 0 4 255); do printf "\e[48;2;%d;%d;%dm \e[0m" $i $((255-i)) 180; done; echo
printf "\e[1mbold\e[0m \e[2mdim\e[0m \e[31mred\e[0m \e[32mgreen\e[0m \e[33myellow\e[0m \e[34mblue\e[0m \e[35mmagenta\e[0m \e[36mcyan\e[0m \e[7minverse\e[0m\n"
