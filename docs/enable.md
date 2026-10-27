# enable：启用应用

按关键词找到应用并启用它（对应 `pm enable`）。常用于恢复被 [disable](disable.md) 禁用的应用；应用本来就已启用时，命令也会成功。

## 用法

~~~text
adbx [-s <serial>] enable <关键词>...
~~~

关键词至少一个。多个关键词必须同时出现在包名中，大小写不敏感。

## 命中规则

命令会先从设备包名中查找目标：

1. 只有一个关键词且它刚好是完整包名时，优先采用精确匹配。
2. 否则按关键词做子串匹配。
3. 唯一命中才会启用应用；命中多个时列出候选并退出，不会盲目操作；零命中时报错。

## 示例

唯一命中：

~~~bash
adbx enable wanandroid
# ✓ 已启用 com.location.wanandroid
~~~

恢复之前禁用的预装应用：

~~~bash
adbx disable com.android.browser  # 完整包名精确命中
adbx enable com.android.browser   # 恢复
~~~

指定设备：

~~~bash
adbx -s 3GKL2NILLN enable wanandroid
~~~

## 相关说明

- [disable](disable.md) 是对应的禁用命令。
- [使用文档首页](README.md) 介绍通用的 serial 和包名匹配规则。
- [常见问题](troubleshooting.md) 说明设备未授权、多设备和关键词歧义。
