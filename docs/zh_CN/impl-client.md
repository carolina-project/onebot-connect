# 客户端实现方式

## 简介

本节将介绍如何实现一个客户端。

## 实现步骤

1. 编写`Connect` trait的实现，实现连接逻辑
    例如WebSocket，通过实现`Connect` 来创建WebSocket连接，同时在后台监听连接

2. 编写`AppProvider` 和`App`
    通过编写`Provider`和`App`实现来和连接通信，提供调用连接接口的能力

3. 编写`MessageSource`
    通过编写`MessageSource`实现来获取连接产生的一系列消息，比如连接关闭和事件。


