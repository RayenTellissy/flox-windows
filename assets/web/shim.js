window.FloxBridge = { onMessage: function (s) { window.webkit.messageHandlers.flox.postMessage(String(s)) } };
