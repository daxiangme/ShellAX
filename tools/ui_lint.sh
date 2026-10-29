#!/bin/sh
# 界面规范检查：业务视图不得直接写字号、圆角、图标尺寸、悬停色等字面量，统一走 ui / theme 中的 token 与组件。
# 个别确需例外的行，在行尾加 `// ui-lint: allow <原因>`。
cd "$(dirname "$0")/.." || exit 1
fail=0
# 定义 token 与组件本身的文件不检查
TOKENS='^src/(ui|theme|card_layout|chrome)\.rs:'
check() {
  name=$1; re=$2; skip=$3
  hits=$(grep -rnE --include='*.rs' "$re" src | grep -vE "$skip" | grep -v 'ui-lint: allow')
  n=$(printf '%s' "$hits" | grep -c .)
  if [ "$n" -gt 0 ]; then echo "✗ ${name}：${n} 处"; printf '%s\n' "$hits" | head -15; fail=1; else echo "✓ ${name}"; fi
}
check "字号字面量 text_size(px(数字))"      'text_size\(px\([0-9]'                                   "$TOKENS"
check "圆角字面量"                          'rounded(_t[lr]|_b[lr])?\(px\([0-9]|rounded_(sm|md|lg|xl|2xl)\(\)' "$TOKENS"
check "图标尺寸字面量"                      'icon\(.*, [0-9]+\.[0-9]*, '                               "$TOKENS"
check "SVG 尺寸字面量"                      'svg\(\).*\.size\(px\([0-9]'                               "$TOKENS|^src/(file_icon|db_tree)\.rs:"
check "悬停底色使用 surface"                'hover\(.*bg\(c\(cur\(\)\.surface[0-2]\)\)'                "$TOKENS"
check "monospace 字面量"                    'font_family\("monospace"\)'                               '^$'
check "界面标签大写"                        'child\(.*to_uppercase\(\)\)'                              "$TOKENS|^src/(tools|db)/"
check "10px 及以下字号"                     'text_size\(px\((10|[1-9])(\.|\))'                         '^$'
check "直接使用 Tooltip::new（请用 ui::tip）"  'Tooltip::new\('                                          '^src/ui\.rs:'
check "anchored 未贴边约束"                 'anchored\(\)' 'snap_to_window_with_margin'
allow=$(grep -rn --include='*.rs' 'ui-lint: allow' src | wc -l | tr -d ' ')
echo "例外标注：${allow} 处（上限 15）"
[ "$allow" -le 15 ] || fail=1
exit $fail
