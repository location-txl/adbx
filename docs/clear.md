# clear：清除应用数据

按关键词找到应用后清除它的全部本地数据。此操作会同时强制停止应用，通常不可恢复。

## 用法

~~~text
adbx [-s <serial>] clear <关键词>...
~~~

关键词至少一个，大小写不敏感；多个关键词必须同时出现在包名中。

## 执行前确认

清除数据会删除登录状态、缓存、设置和应用数据库。先用只读命令确认目标：

~~~bash
adbx info <关键词>
~~~

只有唯一命中或完整包名精确命中时才会执行。命中多个候选时，命令会列出候选并退出，不会选择其中一个。

## 示例

清除唯一命中的应用：

~~~bash
adbx clear wanandroid
# ✓ 已清除 com.location.wanandroid 数据
~~~

用多个关键词缩小范围：

~~~bash
adbx clear location wanandroid
~~~

完整包名会优先精确匹配：

~~~bash
adbx clear com.v1.chat
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
adbx -s 3GKL2NILLN clear wanandroid
~~~

## 相关说明

- [使用文档首页](README.md) 介绍包名匹配和危险操作。
- [常见问题](troubleshooting.md) 说明设备选择和命中结果。
