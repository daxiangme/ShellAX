#!/bin/bash
# font fallback demo: CJK, emoji, Nerd Font icons, Powerline, ligature candidates (UTF-8 literals)
echo '中文：你好，世界！终端字体回退测试  日本語 かな  한국어'
echo 'Emoji: 🚀 ✅ 🔥 🐧 👍 😀   wide-check: |🚀|中|ab|'
echo 'Nerd:   git   rust   docker   branch   folder   term'
printf '\e[44;30m  user \e[34;42m\e[30m  ~/src \e[32;49m\e[0m    Powerline\n'
echo 'No ligatures: -> => != === <= >= :: www ++ |> <!-- fi fl'
echo 'Box: ┌──┬──┐ │▓▒░│ └──┴──┘  Braille: ⣿⡇  Math: ∀∂∈ℝ'
