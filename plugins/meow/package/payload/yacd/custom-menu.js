(function () {
  'use strict';

  var BTN_ID = 'custom-float-btn';
  var BTN_HREF = './sub.html';

  var ICON_SVG = '<svg viewBox="0 0 24 24" width="24" height="24" fill="#fff" xmlns="http://www.w3.org/2000/svg">' +
    '<circle cx="6.18" cy="17.82" r="2.18"/>' +
    '<path d="M4 4.44v2.83c7.03 0 12.73 5.7 12.73 12.73h2.83c0-8.59-6.97-15.56-15.56-15.56z"/>' +
    '<path d="M4 10.1v2.83c3.9 0 7.07 3.17 7.07 7.07h2.83c0-5.47-4.43-9.9-9.9-9.9z"/>' +
    '</svg>';

  function injectFloatButton() {
    if (document.getElementById(BTN_ID)) return true;
    if (!document.body) return false;

    var btn = document.createElement('a');
    btn.id = BTN_ID;
    btn.href = BTN_HREF;
    btn.innerHTML = ICON_SVG;
    btn.style.cssText =
      'position:fixed;' +
      'right:16px;' +
      'bottom:36px;' +
      'z-index:99999;' +
      'width:48px;' +
      'height:48px;' +
      'border-radius:50%;' +
      'background:#60a5fa;' +
      'display:flex;' +
      'align-items:center;' +
      'justify-content:center;' +
      'box-shadow:0 4px 12px rgba(0,0,0,.4);' +
      'text-decoration:none;' +
      'cursor:pointer;' +
      '-webkit-tap-highlight-color:transparent;';

    btn.addEventListener('click', function (e) {
      e.preventDefault();
      e.stopPropagation();
      window.location.href = BTN_HREF;
    }, true);

    document.body.appendChild(btn);
    console.log('[custom-float-btn] injected');
    return true;
  }

  function start() {
    if (!document.body) {
      document.addEventListener('DOMContentLoaded', start);
      return;
    }
    injectFloatButton();
    var observer = new MutationObserver(function () { injectFloatButton(); });
    observer.observe(document.body, { childList: true, subtree: true });
    setTimeout(injectFloatButton, 500);
    setTimeout(injectFloatButton, 1500);
    setTimeout(injectFloatButton, 3000);
  }

  start();
})();
