# disable：禁用应用

按关键词找到应用并禁用它（对应 `pm disable-user`，作用于主用户 0）。适合临时停用预装应用而不卸载；之后随时可以用 [enable](enable.md) 恢复。

禁用不是卸载：应用和它的数据都保留，只是图标消失、无法运行。系统关键应用被禁用时设备会拒绝执行，命令会透传报错原文并以非零退出码结束。

## 用法

~~~text
adbx [-s <serial>] disable <关键词>...
~~~

关键词至少一个。多个关键词必须同时出现在包名中，大小写不敏感。

## 命中规则

命令会先从设备包名中查找目标：

1. 只有一个关键词且它刚好是完整包名时，优先采用精确匹配。
2. 否则按关键词做子串匹配。
3. 唯一命中才会禁用应用；命中多个时列出候选并退出，不会盲目操作；零命中时报错。

## 示例

唯一命中：

~~~bash
adbx disable wanandroid
# ✓ 已禁用 com.location.wanandroid
~~~

输入完整包名时精确命中，不会误禁带更长后缀的包（如同时存在的 `com.android.browser.beta`）：

~~~bash
adbx disable com.android.browser
~~~

命中多个时：

~~~text
✗ 多个包匹配 "setting"：
    com.android.providers.settings
    com.android.settings
  请使用更精确的关键词重试
~~~

指定设备：

~~~bash
adbx -s 3GKL2NILLN disable wanandroid
~~~

## 相关说明

- [enable](enable.md) 随时恢复被禁用的应用。
- [使用文档首页](README.md) 介绍通用的 serial 和包名匹配规则。
- [常见问题](troubleshooting.md) 说明设备未授权、多设备和关键词歧义。
