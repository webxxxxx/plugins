# 打包器与验签器保持同一份实现

`plugin bundle` 永远盖打包机自己的编译期三元组（没有任何 platform/target flag），所以它今天打不出给其它平台的包。我们选择**给 `plugin bundle` 加 `--platform <triple>`**，仓库 CI 调用它，而不是另写一个打包器。

## Considered Options

- **仓库自带打包器**（照 `tools/build_spk.mjs` 的语义重写一份）：否决。manifest 规范化、签名的原样字节、ustar 头、路径排序与权限位都是验签语义的一部分，实现两遍就一定会漂移，漂移的表现是"CI 全绿、用户装不上"这类最难查的故障。
- **每个目标平台在对应的 runner/模拟器上跑一遍 `bundle`**：否决。把平台矩阵变成 runner 矩阵，成本和脆弱度都不可接受，而打包本身并不执行任何 payload。

## Consequences

一期必须先落地 `plugin-manager` 的一次小改动（加 flag），插件仓库的流水线才有得调。这是本期唯一一处"下游等下上"的耦合，需要显式排期。
