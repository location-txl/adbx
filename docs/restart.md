# restart：重启应用

按关键词找到应用，先停止它，再启动它的默认入口。应用需要有可启动的 Launcher Activity。

## 用法

~~~text
adbx [-s <serial>] restart <关键词>...
~~~

关键词至少一个，大小写不敏感；多个关键词是 AND 关系。

## 命中规则

唯一命中或完整包名精确命中时才会执行重启。命中多个会列出候选并退出，零命中会报错。输入完整包名时，精确匹配优先于子串匹配。

## 示例

~~~bash
adbx restart wanandroid
# ✓ 已重启 com.location.wanandroid

adbx restart location wanandroid
~~~

完整包名示例：

~~~bash
adbx restart com.v1.chat
~~~

指定设备：

~~~bash
adbx -s 3GKL2NILLN restart wanandroid
~~~

没有 Launcher Activity 的服务类应用无法通过此命令启动；停止成功后，启动阶段会显示设备返回的错误。

## 相关说明

- [使用文档首页](README.md) 介绍通用包名匹配。
- [常见问题](troubleshooting.md) 说明设备未连接或命中不唯一时的处理方法。
