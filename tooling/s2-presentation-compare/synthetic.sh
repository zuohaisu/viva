#!/usr/bin/env bash
# Known ANSI fixture for the compare harness (not a substitute for real CLI checks).
printf '\033[1;31mbold red\033[0m \033[2mdim\033[0m \033[3mitalic\033[0m \033[4munder\033[0m \033[7minverse\033[0m\n'
printf '\033[38;5;208m256-orange\033[0m \033[38;2;10;200;30mtruecolor\033[0m \033[48;5;21;97mbg-blue\033[0m\n'
printf '\033[9mstrike\033[0m \033[4:3mcurly\033[0m\n'
printf 'CJK: 中文字符宽度 ｆｕｌｌｗｉｄｔｈ emoji: 😀 e\xcc\x81 combining\n'
printf 'long line: %s\n' "$(printf 'x%.0s' $(seq 1 150))"
printf '\033[?25l\033[10;20H<-hidden cursor parked here\n'
printf '\033[?2004h\033[?1h\033[?1000h\033[?1006h'
sleep 1
printf '\033[?1049h\033[2J\033[H\033[1;32malt screen content\033[0m\033[5;10H\033[?25h'
sleep 5
