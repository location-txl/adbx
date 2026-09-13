# info：查看应用信息

按关键词查找应用，显示完整包名、versionName 和 versionCode。这个命令只读，不会修改设备。

## 用法

~~~text
adbx [-s <serial>] info <关键词>...
~~~

关键词至少一个，大小写不敏感；多个关键词是 AND 关系。

## 命中规则

1. 单个关键词刚好等于完整包名时，优先精确匹配。
2. 其他情况按关键词做子串匹配。
3. 唯一命中显示一项；命中多个时全部显示；零命中时报错。

## 示例

唯一命中：

~~~bash
adbx info userdictionary
~~~

输出类似：

~~~text
✓ com.android.providers.userdictionary
    versionName: 7.1.2
    versionCode: 25
~~~

多命中时会全部展示：

~~~bash
adbx info setting
~~~

设备或系统输出缺少版本字段时会显示“未知”，不会因为字段缺失而失败。

指定设备：

~~~bash
adbx -s 3GKL2NILLN info userdictionary
~~~

## 相关说明

- [使用文档首页](README.md) 介绍包名匹配。
- [常见问题](troubleshooting.md) 说明无设备、多个设备和关键词问题。
