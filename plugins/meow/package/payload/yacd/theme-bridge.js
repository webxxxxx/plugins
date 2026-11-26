// 把 YACD 自己的主题告诉外层页面：两边不同源，读不到彼此的 DOM / localStorage，
// 只能这样传。插件页面（payload/web/index.html）顶部的标题栏就跟着它变。
//
// YACD 把主题写在 <html data-theme="dark|light|auto"> 上（右上角的主题开关改的就是它），
// auto 按系统偏好折算成实际的一档。只在值真的变了的时候发一次。
(function () {
  'use strict';

  var root = document.documentElement;
  var mq = window.matchMedia ? window.matchMedia('(prefers-color-scheme: dark)') : null;
  var last = '';

  function current() {
    var value = root.getAttribute('data-theme');
    if (value === 'dark' || value === 'light') return value;
    return mq && mq.matches ? 'dark' : 'light';
  }

  function report() {
    var theme = current();
    if (theme === last) return;
    last = theme;
    try {
      window.parent.postMessage({ type: 'meow:theme', theme: theme }, '*');
    } catch (e) {
      // 父页面不在（直接打开 /ui/ 的时候），什么都不用做。
    }
  }

  function start() {
    report();
    // data-theme 是 YACD 自己写的，盯着属性变化就够了。
    new MutationObserver(report).observe(root, { attributes: true, attributeFilter: ['data-theme'] });
    if (mq && mq.addEventListener) mq.addEventListener('change', report);
    // YACD 挂载之后才写这个属性：前几秒补几次，别让上报只依赖观察器一次机会。
    [200, 800, 2000].forEach(function (ms) { setTimeout(report, ms); });
  }

  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', start);
  else start();
}());
