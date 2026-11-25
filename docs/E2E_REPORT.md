# Real teaching API acceptance

Run: 2026-11-01T01:26:28+08:00 (Asia/Shanghai).

This is an internal typed acceptance scenario through the real local HTTP API and authenticated Kimi Code 2.3.1. Module inventories and trial outcomes below are synthetic scripted reports entered by the test harness through the manual-report API; no adult observation, child session, or toy-car hardware execution is claimed. The live camera service remains running independently. This script acknowledges received teaching text as shown; it does not substitute for a human voice/UI demonstration. Fixtures are visibly named internal acceptance profiles and use a separate learner identity from any actual learner.

| Check | Status | Evidence |
|---|---|---|
| Real Kimi verified and available | PASS | version=2.3.1 |
| Inventory keeps manual provenance | PASS | hardware_verified remains false |
| Reported success is not relabelled failure | PASS | 学习者报告第一次试跑已成功停车并亮绿灯，但觉得离墙太近，希望增大间隙；尚无硬件验证。 |
| Session-only preference not promoted to permanent trait | PASS |  |
| Fast memory contains actually delivered hint | PASS |  |
| Slow memory has source-linked reported progress | PASS |  |
| Slow memory retains unfinished wall experiment | PASS |  |
| New session treats current module build as unknown | PASS |  |
| New session recalls source-linked previous progress | PASS |  |
| Learner B has no A dialogue or slow memories | PASS |  |
| Switch cancels A generation and keeps B empty | PASS |  |
| Correction fixture produced genuine candidate memory | PASS |  |
| Teacher correction replaces active note | PASS |  |
| Delete removes memory and derived context | PASS |  |

## Actual dialogue (anonymous internal scenario)

### 1. Goal — 15110 ms

Input: 我想让小车到墙前停下来，再亮绿灯。

Real Kimi: 可以实现：用距离模块测量与墙的距离，控制模块判断是否够近，再让马达停止并点亮绿灯。先确认一下：你现在接了哪些模块，距离模块和绿灯分别接在哪里？

### 2. Missing capability — 8281 ms

Input: 老师刚才手动确认我有主控、电机和灯。我还缺什么能力？请给我一个提示。

Real Kimi: 还缺“测量距离”的能力，需要距离模块。它能测出小车与墙的距离，帮助主控判断何时停车并亮绿灯。

### 3. Learner connection report — 18516 ms

Input: 我把测距模块接好了。这只是我的口头报告，还没有硬件验证。

Real Kimi: 好的，目前只能记为你口头报告已接好测距模块，还没有硬件验证。下一步先做一个简单测试：改变小车与墙的距离，观察测距数值是否跟着变化；你看到了什么？

### 4. Success with improvement — 22578 ms

Input: 第一次试跑，它能停下来，也亮绿灯了，但是我觉得离墙太近，想要大一点的间隙。请记住第一次据我报告已经成功。

Real Kimi: 已记住：据你报告，第一次试跑成功停车并亮绿灯，只是间隙偏小。下一步只把停车距离的设定调大一些，其他保持不变，再试跑一次；这次间隙是否更大了？

### 5. Session help preference — 13875 ms

Input: 这次一次只告诉我一步，我想让它更早停。

Real Kimi: 只把停车距离的设定调大一些，其他保持不变。

### 6. Comparison and pending experiment — 24437 ms

Input: 我只调了停车距离，其他条件一样。第二次据我观察提前停下，亮绿灯，离墙间隙更大了。请记住这个进展；下次我想移动墙的位置，再测试是否仍按距离停车，这个实验还没有做。

Real Kimi: 已记住：据你观察，第二次只调整停车距离后，小车更早停下、绿灯亮，离墙间隙也更大了；第一次仍是成功试跑。下次可以只移动墙的位置，其他保持不变，比较它是否仍按设定距离停车；这个实验尚未进行。

### 7. Resume from memory — 16969 ms

Input: 继续上次的小车停车距离活动。上次我还留下了什么没做？请先确认今天搭建。

Real Kimi: 上次还没做的是：只移动墙的位置，其他条件保持不变，测试小车是否仍按设定距离停车。先确认今天的搭建：控制、马达、距离和灯光模块，以及小车和墙，现在都接好并放在测试位置了吗？

### 8. Correction fixture through real Kimi — 12016 ms

Input: 请记住我的口头进展：小车已经停车并亮绿灯，但没有做移动墙实验。

Real Kimi: 已记住：据你口头报告，小车已经停车并亮绿灯；移动墙实验尚未进行。
