# catalog 是 OSS 上的静态 JSON 树，不是服务

商城与管理器都需要知道"有哪些插件、哪个版本、支持哪个平台"，但管理器目前没有任何远程索引概念。我们把 catalog 定为 **OSS/CDN 上的静态 JSON 树**：全量 `catalog/v1/index.json` 加每插件分片 `catalog/v1/<name>.json`。

## Considered Options

- **索引寄生在 GitHub**（release 资产或 raw 文件），OSS 只放 `.spk`：否决。发布被劈成两个系统，CI 要写两处，且 GitHub 的可用性会变成商城可用性的天花板。
- **动态 API 服务**（数据库 + 查询接口）：否决。当前没有需要动态查询的需求，纯属自找运维。

## Consequences

静态文件运维成本近零、天然可缓存、可整体回滚，并且同时服务商城前端和以后的 `plugin search`。

代价有两个：撤回是"重写 JSON"而不是一次数据库写，需要保证重写是原子的；以后真要做搜索、评分、推荐，只能另加服务，catalog 本身不会被改造成数据库。
