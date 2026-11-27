/* 应用插件菜单注入（由注入端内联到厂商 app.js，幂等：老 plugin 服务的 web_menu()，
 * 新宿主是 webbind 插件）
 * v14：面板 iframe 指向 svcd 控制台 http://<host>:8787/（新的插件宿主）；
 *   打开面板前先用浏览器里的厂商会话换一个 svcd 会话（POST /vendor-login）：
 *   厂商后台已经登录时，:8787 的控制台不用再登录一次。
 *   厂商会话绑客户端 IP，服务端代查不可行；所以“是否已登录”由这里（厂商
 *   同源页面，带着真实 SID）用 AJAX 问，确认 SUCC 才去换会话。
 * v13：克隆厂商原生菜单项，末尾追加到 .menu_items（「系统」下面，滚动区里）。
 *   容器：ul.menu_bar.el-menu > div.menu_items（v-for 生成的项）
 *   每项：div > a > li.el-menu-item > img.common + img.focus + span
 * 面板：贴在 .sidebar-container 右侧；窄屏（<=992px）铺满并收起抽屉
 * 三条铁律（前面踩过，直接白屏 / 菜单点不动）：
 *   1. 不用 MutationObserver 监听整棵树 —— 会在 Vue 打补丁过程中同步插/删节点；
 *   2. 绝不删 Vue 引用链上的节点 —— 路由切换时会让 Vue 的 removeChild 抛错；
 *   3. 只能追加在容器**最后** —— Vue 按下标配对，插在中间会把后面兄弟的 vnode 顶错位。
 * 登录页没有 ul.menu_bar，所以不会误注入（不靠密码框之类的猜测）。
 * 本文件不使用正则，也不使用转义字符。 */
(function () {
  // 注意：不要复用 v8 的 window.__srPluginMenu —— 旧脚本如果先跑过，
  // 那个标记会让这里直接 return，于是新版本永远不生效，页面上只剩 v8 的裸 div。
  if (window.__srPluginMenuV9) return;
  window.__srPluginMenuV9 = 1;

  var LABEL = '应用插件';
  // svcd 控制台（新插件宿主）：默认 bind 0.0.0.0:8787，和厂商后台同主机、不同端口。
  // 设备没有 TLS，所以固定 http://；厂商页面若走 https，iframe 会被混合内容拦住。
  var HOST = 'http://' + location.hostname + ':8787';
  var URL = HOST + '/';
  var AUTH = HOST + '/vendor-login';
  // 厂商后台的登录态（只认浏览器里带 SID 的请求，见 vendorLogin）
  var VENDOR_STATUS = '/?_type=vueData&_tag=home_internetreg_lua_no_update_sess&_=';
  var handoff = 0;   // 1 = 这次页面生命周期里已经换到 svcd 会话
  var opening = 0;   // 防止连点两次菜单同时开面板
  var ICON = '/jquery/static/img/menu/icon_nav_application.svg';
  var ICON_ON = '/jquery/static/img/menu/focus/icon_nav_application.svg';
  var ID = 'sr-plugin-menu-item';
  var FRAME = 'sr-plugin-frame';

  function log(m) { try { console.log('[srfix-menu] ' + m); } catch (e) {} }

  // 左侧菜单的 <ul>（厂商后台登录后才会有）
  function menuBox() {
    return document.querySelector('ul.menu_bar') || document.querySelector('ul.el-menu');
  }

  // 模板所在的滚动容器（v-for 生成的项都在这里面）
  function itemHost(box) {
    return (box && box.querySelector('.menu_items')) || box;
  }

  // 拿一个原生项当模板：取最后一个带 li.el-menu-item 的兄弟节点
  function nativeItem(host) {
    var kids = host.children, i, c;
    for (i = kids.length - 1; i >= 0; i--) {
      c = kids[i];
      if (!c || c.id === ID) continue;
      if (c.querySelector && c.querySelector('li.el-menu-item')) return c;
    }
    return null;
  }

  // 厂商后台是否已登录：带着浏览器的 cookie 问厂商自己的 loginStatus。
  // 只有浏览器里这么问才准 —— 厂商会话绑客户端 IP，服务端代查会 404。
  function vendorLoggedIn() {
    return fetch(VENDOR_STATUS + Date.now(), {credentials: 'include', cache: 'no-store'})
      .then(function (r) { return r.text(); })
      .then(function (t) { return (t || '').indexOf('SUCC') >= 0; })
      .catch(function () { return false; });
  }

  // 厂商已登录 -> 让 :8787 的 svcd 控制台免登录。
  // 跨端口、同主机：cookie 是同域的（按主机存，不按端口），svcd 的
  // /vendor-login 把厂商会话换成一个 svcd 会话，iframe 里的控制台直接就是登录态。
  // 换不到（没登录 / svcd 没开这个入口）就照常打开，控制台会显示自己的登录页。
  function vendorLogin(done) {
    if (handoff) { done(); return; }
    vendorLoggedIn().then(function (ok) {
      if (!ok) { done(); return; }
      return fetch(AUTH, {
        method: 'POST',
        credentials: 'include',
        headers: {'X-Vendor-OK': '1'}
      }).then(function (r) {
        if (r.ok) { handoff = 1; log('svcd handoff ok'); }
      }).catch(function () {}).then(done);
    }).catch(function () { done(); });
  }

  // 窄屏（<=992px，和厂商 CSS 断点一致）时侧边栏是抽屉
  function narrow() { return window.innerWidth <= 992; }

  // 点厂商自己的汉堡按钮 / 遮罩来收放抽屉，保持它内部状态一致
  function clickFirst(sels) {
    for (var i = 0; i < sels.length; i++) {
      var el = document.querySelector(sels[i]);
      if (el) { try { el.click(); return true; } catch (e) {} }
    }
    return false;
  }

  // 收起抽屉：原生菜单点完厂商会自己收，我们那项不是 Vue 组件，得手动触一下
  function closeDrawer() {
    if (!narrow()) return;
    if (clickFirst(['.drawer-bg', '.hamburger-container', '.hamburger'])) return;
    var wrap = document.querySelector('.app-wrapper');
    if (wrap && (wrap.className || '').indexOf('openSidebar') >= 0) {
      wrap.className = wrap.className.replace('openSidebar', 'hideSidebar');
    }
  }

  // 重新打开抽屉（窄屏时面板上的「返回」用得着）
  function openDrawer() {
    if (!narrow()) return;
    clickFirst(['.hamburger-container', '.hamburger']);
  }

  // 面板就贴在侧边栏右侧；窄屏时侧边栏是抽屉，面板铺满整宽
  function panelLeft() {
    if (narrow()) return 0;
    var sb = document.querySelector('.sidebar-container');
    var w = sb ? sb.getBoundingClientRect().width : 0;
    return w > 0 ? Math.round(w) : 280;
  }

  // 高亮：打开面板时把我们那项点亮、把厂商当前项的高亮摘掉；
  // 关掉面板时还原。纯样式操作，出错只影响观感。
  var savedActive = [];

  function setActive(on) {
    var me = document.getElementById(ID);
    var mine = me ? me.querySelector('li') : null;
    var i, o;
    if (on) {
      savedActive = [];
      var others = document.querySelectorAll('ul.menu_bar li.el-menu-item.is-active');
      for (i = 0; i < others.length; i++) {
        o = others[i];
        if (o === mine) continue;
        savedActive.push({ el: o, color: o.style.color });
        o.classList.remove('is-active');
        o.style.color = 'rgb(255, 255, 255)';
      }
      if (mine) {
        mine.classList.add('is-active');
        mine.style.color = 'rgb(61, 163, 247)';
      }
    } else {
      if (mine) {
        mine.classList.remove('is-active');
        mine.style.color = 'rgb(255, 255, 255)';
      }
      for (i = 0; i < savedActive.length; i++) {
        o = savedActive[i];
        try {
          o.el.classList.add('is-active');
          o.el.style.color = o.color;
        } catch (e) {}
      }
      savedActive = [];
    }
  }

  function closePanel() {
    var box = document.getElementById(FRAME);
    if (box && box.parentNode) box.parentNode.removeChild(box);
    try { setActive(false); } catch (e) {}
  }

  // 打开面板前先换 svcd 会话：厂商后台已登录时，:8787 的控制台不用再登录。
  // 换不到也照常打开（控制台会显示它自己的登录页），这条路不能挡住入口。
  function openPanel() {
    if (document.getElementById(FRAME) || opening) return;
    opening = 1;
    vendorLogin(function () {
      opening = 0;
      showPanel();
    });
  }

  function showPanel() {
    if (document.getElementById(FRAME)) return;
    var left = panelLeft();
    var box = document.createElement('div');
    box.id = FRAME;
    box.style.cssText = 'position:fixed;top:0;right:0;bottom:0;left:' + left + 'px;' +
                        'background:#f4f6f8;z-index:99998;overflow:hidden';
    var f = document.createElement('iframe');
    f.src = URL;
    f.style.cssText = 'display:block;width:100%;height:100%;border:0';
    box.appendChild(f);
    // 窄屏：面板铺满，抽屉必须收起；再给一个「返回」把抽屉叫回来
    if (narrow()) {
      try { closeDrawer(); } catch (e) {}
      var back = document.createElement('div');
      back.textContent = '返回';
      back.style.cssText = 'position:absolute;left:12px;top:12px;z-index:2;' +
                           'background:rgba(0,0,0,.55);color:#fff;padding:7px 14px;' +
                           'border-radius:18px;font-size:13px;cursor:pointer';
      back.onclick = function () { closePanel(); try { openDrawer(); } catch (e) {} };
      box.appendChild(back);
    }
    document.body.appendChild(box);
    try { setActive(true); } catch (e) {}
    log('panel open, left=' + left);
  }

  function stripFab() {
    var fab = document.getElementById('sr-plugin-fab');
    if (fab && fab.parentNode) fab.parentNode.removeChild(fab);
  }

  function inject() {
    var box = menuBox();
    if (!box) return false;
    var host = itemHost(box);
    var cur = document.getElementById(ID);
    // 旧版（v8）留下的是个裸 div：没有 li，丢掉重建，不能就这样认领
    if (cur && !cur.querySelector('li.el-menu-item')) {
      if (cur.parentNode) cur.parentNode.removeChild(cur);
      cur = null;
    }
    if (cur) {
      // 被挪到别处（侧边栏重建）就挂回去；只允许挂在宿主容器的末尾
      if (cur.parentNode !== host) host.appendChild(cur);
      stripFab();
      return true;
    }
    var tpl = nativeItem(host);
    if (!tpl) return false;

    var item = tpl.cloneNode(true);
    item.id = ID;
    item.setAttribute('role', 'menuitem');
    item.title = 'srfix: ' + (box.className || '') + ' / ' + tpl.tagName;

    var a = item.querySelector('a');
    if (a) {
      a.className = '';                                  // 去掉 router-link-active
      a.removeAttribute('aria-current');
      a.setAttribute('href', 'javascript:;');            // 不走 Vue 路由
    }
    var li = item.querySelector('li');
    if (li) {
      li.className = 'el-menu-item submenu-title-noDropdown';   // 去掉 is-active
      li.style.paddingLeft = '20px';
      li.style.color = 'rgb(255, 255, 255)';
    }
    var imgs = item.querySelectorAll('img');
    if (imgs.length > 0) imgs[0].src = ICON;
    if (imgs.length > 1) imgs[1].src = ICON_ON;
    var span = item.querySelector('span');
    if (!span) {
      span = document.createElement('span');
      (li || item).appendChild(span);
    }
    span.textContent = LABEL;

    // 捕获阶段先拦住，避免冒泡到标题里的 <a> 触发跳转
    item.addEventListener('click', function (e) {
      e.preventDefault();
      e.stopPropagation();
      if (document.getElementById(FRAME)) closePanel(); else openPanel();
    }, true);

    // 末尾追加（v8 的位置：滚动区里「系统」下面）。
    // Vue 的 updateChildren 是按下标配对的，只能追加在最后：
    // 追加项会被当成"多出来的节点"删掉（我们再补），不会错位到别的 vnode 上。
    host.appendChild(item);
    stripFab();
    log('injected into ' + (box.className || 'ul.menu_bar'));
    return true;
  }

  // 不再做悬浮兜底按钮；stripFab() 负责把旧版本留下的清掉。
  // tick 全部包在 try 里：这段代码绝不能把厂商 SPA 弄挂。
  function tick() {
    try {
      inject();
      stripFab();
    } catch (e) {
      log('tick error: ' + (e && e.message ? e.message : e));
    }
  }

  // 点了别的菜单项（hash 变了）就把我们的面板收起来，恢复厂商页面
  window.addEventListener('hashchange', function () {
    try { closePanel(); } catch (e) {}
  });

  // 面板开着时点任何"不是面板、也不是我们那项"的地方（典型是左侧别的菜单）：
  // 先把面板收起，但**不拦截**这次点击，让厂商自己的路由照常处理。
  // 放在捕获阶段，保证在厂商的 click 处理之前跑完。
  document.addEventListener('click', function (e) {
    if (!document.getElementById(FRAME)) return;
    var t = e.target;
    var me = document.getElementById(ID);
    var box = document.getElementById(FRAME);
    if (me && (me === t || me.contains(t))) return;
    if (box && (box === t || box.contains(t))) return;
    try { closePanel(); } catch (err) {}
  }, true);
  // 侧边栏宽度会随窗口变，面板左边距跟着改
  window.addEventListener('resize', function () {
    try {
      var box = document.getElementById(FRAME);
      if (box) box.style.left = panelLeft() + 'px';
    } catch (e) {}
  });

  // 只用定时器，不用 MutationObserver：
  // 监听整棵树会在 Vue 补丁过程中同步插/删节点，直接把 SPA 搞崩（白屏）。
  setInterval(tick, 500);
  document.documentElement.setAttribute('data-sr-menu', 'v14');
  tick();
  log('v14 armed');
})();
