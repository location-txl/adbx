# uninstall：卸载应用

按关键词找到应用并卸载。卸载会删除应用本体及其数据，通常不可恢复。

## 用法

~~~text
adbx [-s <serial>] uninstall [-y|--yes] <关键词>...
~~~

- 关键词至少一个；多个关键词必须同时出现在包名中。
- -y 或 --yes 跳过确认，适合已验证目标的脚本场景。
- -s 指定设备 serial。

## 安全确认

默认情况下，唯一命中后会显示：

~~~text
即将卸载 com.location.wanandroid，确认？[y/N]
~~~

输入 y 或 yes（不区分大小写）才会继续；直接回车、输入其他内容或按 Ctrl-D 都会取消，取消不会被视为失败。

使用 -y 前，建议先执行：

~~~bash
adbx info <关键词>
~~~

确认输出只有目标应用。

## 命中规则

完整包名精确匹配优先。唯一命中才会进入卸载流程；命中多个时会列出候选并退出，零命中会报错。

## 示例

交互确认：

~~~bash
adbx uninstall wanandroid
# 输入 y 后：
# ✓ 已卸载 com.location.wanandroid
~~~

跳过确认：

~~~bash
adbx uninstall -y wanandroid
~~~

取消卸载：

~~~text
已取消卸载 com.location.wanandroid
~~~

指定设备：

~~~bash
adbx -s 3GKL2NILLN uninstall -y wanandroid
~~~

## 相关说明

- [使用文档首页](README.md) 介绍包名匹配和不可恢复操作。
- [常见问题](troubleshooting.md) 说明设备选择和关键词歧义。
