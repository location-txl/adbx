# stop：停止应用

按关键词找到应用后强制停止它。应用本来就没有运行时，命令也会成功。

## 用法

~~~text
adbx [-s <serial>] stop <关键词>...
~~~

关键词至少一个。多个关键词必须同时出现在包名中，大小写不敏感。

## 命中规则

命令会先从设备包名中查找目标：

1. 只有一个关键词且它刚好是完整包名时，优先采用精确匹配。
2. 否则按关键词做子串匹配。
3. 唯一命中才会停止应用；命中多个时列出候选并退出，不会盲目操作；零命中时报错。

## 示例

唯一命中：

~~~bash
adbx stop wanandroid
# ✓ 已停止 com.location.wanandroid
~~~

用多个关键词缩小范围：

~~~bash
adbx stop location wanandroid
~~~

输入完整包名时不会误停带有更长后缀的包：

~~~bash
adbx stop com.v1.chat
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
adbx -s 3GKL2NILLN stop wanandroid
~~~

## 相关说明

- [使用文档首页](README.md) 介绍通用的 serial 和包名匹配规则。
- [常见问题](troubleshooting.md) 说明设备未授权、多设备和关键词歧义。
