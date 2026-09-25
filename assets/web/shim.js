window.FloxBridge = { onMessage: function (s) { try { window.chrome.webview.postMessage(String(s)) } catch (e) {} } };
