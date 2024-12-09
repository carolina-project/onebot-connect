# onebot-connet

提供[OneBot Connect](https://12.onebot.dev/connect/)中的通信实现。

## 协议转换

由于OneBot11与12差异较大，所以只提供11转12的功能，并且OneBot11侧必须有**能够同时提供调用API和事件接收能力**的连接

以下连接能够满足要求:

-   HTTP + HTTP POST
-   正向Websocket
-   反向Websocket
