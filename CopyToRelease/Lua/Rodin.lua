-- 3D→2D ViewProjection 矩陣 + 敵人 ESP (螢幕中心頂 ↔ 敵人 z+100 線) 與 3D 方塊
-- 位置: APlayerCameraManager 相機快取 +0x530 (世界, cm)
-- 位向: CameraCache POV (pcm+0x548, FRotator 角度) — 與位置和 FOV 同源, 含開鏡 ADS 位移與呼吸 sway
-- 為什麼不能用 ctrl+0x4E0 (ControlRotation): 它是純輸入朝向, 不疊加開鏡時實際渲染用的位移與 sway。
-- 實測: 腰射時兩者完全一致 (−6.08185 / −98.60224, 七位小數相同); 按住開鏡後 POV 出現約 +0.64°/+0.43° 偏差。
-- 這個角誤在腰射(fov 80)下幾乎不被察覺, 開鏡 3x (fov 26.67) 後放大約 3.5 倍, 投影落點偏移幾十像素,
-- 且隨 sway 相位每次開鏡不同, 看起來像「隨機歪」; 誤差一併與目標距離成反比 (1/z), 走近時更甚。
-- 修復原則: 位置/旋轉/FOV 必須從同一個 CameraCache POV 讀, 渲染什麼就投影什麼
-- FOV: 實際渲染 FOV 在 CameraCache POV (pcm+0x554): 腰射 80.0, 開鏡 lerp 到武器倍率; ctrl+0x2B14 只是基礎 FOV, 不採用
-- 敵人列舉: 關卡 Actors TArray (GWorld +0x30 PersistentLevel -> +0xA0 Data, +0xA8 Num);
--   以角色標記 (STExtraBaseCharacter) 篩選: actor+0x2CD8 == 479.5f (C++ 源碼鏈);
--   再讀 RootComponent +0x208 -> RelativeLocation +0x1E4 得世界座標
-- 自身跳過; 死亡 (State +0x1070, 命中 {262144,262152,1048576,1048592}) 與隊友 (TeamID +0x9A8) 跳過;
--   TeamID<=0 跳過; 人機 (0xA50/0xA51/0xA52/0xA69 任一非 0) 畫灰線, 真人畫紅線
--
-- Aimbot (aim 模組): 配置為捷徑後與遊戲開火鍵重疊; 短捷徑只是「影響器」, 實際自瞄邏輯都在 Tick 內處理
--   走訪所有可見敵人 → 取螢幕中心距離最近者 → 已快取目標: 直接追蹤滑動
--   否則: 立方體螢幕投影 MinMax 包圍盒, 中心點落入才接獲為目標並追蹤
--   (enemies 全詳細計算只算一次, ESP 與 aim 模組共用)
--   接獲/失去目標的判定只在「非快取目標」時進行; 快取目標只要使用者不鬆開捷徑就一直自瞄
--   倍率 Y=0 = 只水平 (用戶自行壓槍), X 依舊照常工作
--   自瞄手指固定 slot 9, 連點手指固定 slot 8 (FullyAutoTrigger / AutoTrigger用), 與使用者手指不競爭; 使用者放手 (整個 Tick 缺席) 時下一幀抬手指
--   Aimbot_Pressing(x, y): x,y = 捷徑按下時手指的屏幕物理像素 (左上原點); 直接落點快取, 供FullyAutoTrigger/AutoTrigger共用; 無 xy 則落點清空
--   AutoTrigger(): 開關式函式 (回 Bool), 捷徑呈按鈕開關; 目標點對準相機 Forward 軸線即自動進入連點循環 (Aimbot 未按下才生效)
local module_candidates = { "libue4.so", "UE4", "Unreal", "Game", "PUBGM" }

local CTRL_PATTERN = '[[[[[["%s"+0xFC70A70]+0x810]+0x78]+0x38]+0x78]+0x30]'
local ONSELF_PATTERN = '[[[[[[["%s"+0xFC70A70]+0x810]+0x78]+0x38]+0x78]+0x30]+0x29F8]'
local GWORLD_PATTERN = '[[["%s"+0xFC70A70]+0x810]+0x78]'

local TEAM_ID_OFF = 0x9a8
-- 死亡判定: State (actor+0x1070) int; 命中即視為死亡/盒; 任意讀不到即保留
local STATE_OFF = 0x1070
local DEAD_STATES = {
    [262144] = true, [262152] = true, [1048576] = true, [1048592] = true,
}
local function is_dead_state(s)
    return s ~= nil and DEAD_STATES[s] == true
end
-- 角色標記: 玩家 pawn 為 479.5f (stextrabasecharacter 共有常量), 區分 STExtraBaseCharacter AI 與絕大部分載具/物資
local PLAYER_MARK_OFF = 0x2cd8
local PLAYER_MARK = 479.5
-- IsBot: 多個候選 byte 任一非 0 即人機
local IS_BOT_OFF = 0xa69
local BOT_OFF_50 = 0xa50
local BOT_OFF_51 = 0xa51
local BOT_OFF_52 = 0xa52
local function is_bot_flag(actor)
    local b = theraohe.read_u8(actor + IS_BOT_OFF) or 0
    if b ~= 0 then return true end
    b = theraohe.read_u8(actor + BOT_OFF_50) or 0
    if b ~= 0 then return true end
    b = theraohe.read_u8(actor + BOT_OFF_51) or 0
    if b ~= 0 then return true end
    b = theraohe.read_u8(actor + BOT_OFF_52) or 0
    return b ~= 0
end
-- RootComponent 內: RelativeLocation +0x1E4, RelativeRotation +0x1F0 (FRotator, 角度)
local RELATIVE_ROTATION_OFF = 0x1f0
local MESH_OFF = 0x510
-- 引擎每幀維護的 ComponentToWorld FTransform (inline, mesh+0x210; 與 C++ 專案 pubg 模式一致)
local COMPONENT_TO_WORLD_OFF = 0x210
-- mesh+0xA30 內聯 TArray<FTransform> (bone component-space, 骨索引索引 48B FTransform), Num @ +0xA38,
--   實際元素起始在 Data+0x30 (C++ 解法把該跳過首元素 48B)
local CACHED_COMPONENT_SPACE_OFF = 0xa30
local BONES_DATA_BIAS = 0x30
local FTRANSFORM_SIZE = 48
-- 姿勢骨索引 (與 C++ 專案 pubg 模式一致): 頭/胸/盆/左肩/右肩/左肘/右肘/左腕/右腕/左大腿/右大腿/左膝/右膝/左踝/右踝
local BONE_TABLE_STANDARD = { 5, 4, 1, 11, 32, 12, 33, 63, 62, 52, 56, 53, 57, 54, 58 }
local BONE_TABLE_68 = { 5, 4, 1, 11, 33, 12, 34, 13, 35, 55, 59, 56, 60, 57, 61 }

-- 自瞄 (Lua 頁面公開)
-- AimbotBone (0-based, 15 項繪製表順序: 頭=0 ... 右踝=14), 橙色連線; <0 = 不高亮。
-- 繪製高亮與自瞄追蹤共用; 0 = 頭 (C++ 專案預設自瞄部位)
AimbotBone = 0
-- 自瞄手指與行為 (Lua 頁面公開, 物理像素座標)
-- 手指起手位: 該處有空白遊戲區域最佳; 會自動鉗位到螢幕內
AimbotStartX = 2000
AimbotStartY = 200
-- AimbotMultiplier (螢幕中心-敵人位置差 * 倍率 = 滑動距離); 涵蓋速度與鏡頭靈敏度
-- Y=0 = 只水平瞄準 (自行壓槍), X 仍照常工作
AimbotMultiplierX = 0.05
AimbotMultiplierY = 0.0
-- 單次滑動像素上限: 本次 dx,dy 合向量超過此值則忽略本幀滑動 (手指不落/不動), 每次忽略 print; <=0 = 不限制
AimbotMaxDrag = 0
-- 畫自瞄用立方體 MinMax 包圍盒 (黃框, 所有可見敵人); ShowCenterCross 畫螢幕中心十字 (綠)
ShowAimbotRect = true
ShowCenterCross = true
-- 開火 (Lua 頁面公開): 與捷徑同按, 在 Aimbot_Pressing 收到的位置循環 按下→間隙→鬆開
-- 連點手指固定 slot 8; 預設間隙 50ms; FireGap同時用於AutoTrigger
FullyAutoTrigger = false
FireGap = 0.05
-- AutoTrigger閥值: 瞄準目標點到相機 Forward 軸線之垂距小於此值 (cm) 即進入連點循環 (30cm 實測值)
AutoTriggerThreshold = 30
-- 雷達 (Lua 頁面公開): Yaw 跟隨雷達 — 中心=自己 (相機 POV 位置), 上=相機視角 Yaw; 把敵人 world 映射到正方形上
-- 雷達位置 (螢幕物理像素, 中心對齊): 預設 800,350; 尺寸=單邊 px (預設 350); 範圍=單邊對應世界長度 (米, 預設 240 → ±120m)
RadarPosX = 800
RadarPosY = 350
RadarSize = 350
RadarRange = 240

-- 自瞄內部狀態 (不反射)
local aim_finger_down = false   -- 自瞄手指是否已落下
local aim_x, aim_y = nil, nil   -- 自瞄手指當前位置; nil = 未落下
local aim_target = nil          -- 已快取自瞄目標 (actor ptr 整數)
local aim_last_call = -1        -- 最後一次 Aimbot_Pressing 的 tick 序號; 使用者放手時 => ticks 跳號
local aim_requested = false     -- 捷徑按住狀態 (影響器); 實際自瞄邏輯在 Tick 的 aim 模組執行
local aim_press_x, aim_press_y = nil, nil  -- 捷徑按下的位置 (物理像素); 不帶則 Aimbot_Pressing 無 xy
-- FullyAutoTrigger內部狀態 (不反射)
local fire_finger_down = false
local fire_down_at = 0.0        -- 本次按下的時刻 (theraohe.now())
local fire_x, fire_y = nil, nil -- 本次按下的位置
-- AutoTrigger開關 (不反射); AutoTrigger 函式點擊翻轉並回傳, 捷徑將其渲染為開關式按鈕; 默認關; 需先按一次捷徑播種落點
local autotrigger_on = false

local ticks = 0

local function resolve_first(pattern)
    for _, pat in ipairs(module_candidates) do
        local p = theraohe.resolve_chain(string.format(pattern, pat))
        if p then
            return p
        end
    end
    return nil
end

function Begin()
    print("[pubgm_global] select com.tencent.ig")
    theraohe.select("com.tencent.ig")
end

local DEG = math.pi / 180

-- 無分支 HSV→RGB (s=v=1, clamp 三角波); 於 25 個整數值繞色環一圈
-- n 任意整數; Lua % 對負數回非負
local floor, abs = math.floor, math.abs
local min, max = math.min, math.max
local function channel(h, offset)
    return min(1, max(0, abs(((h + offset) % 1) * 6 - 3) - 1))
end
local function int_to_rgb(n)
    local h = ((n - 1) % 25) / 25
    local r = channel(h, 0)
    local g = channel(h, -1/3)
    local b = channel(h, -2/3)
    return floor(r * 255 + 0.5), floor(g * 255 + 0.5), floor(b * 255 + 0.5)
end

-- FRotator (角度) → 行向量基底; UE4 世界: X 前 Y 右 Z 上
local function rotator_axes(pitch, yaw, roll)
    local p, y, r = pitch * DEG, yaw * DEG, roll * DEG
    local SP, CP = math.sin(p), math.cos(p)
    local SY, CY = math.sin(y), math.cos(y)
    local SR, CR = math.sin(r), math.cos(r)
    local fx, fy, fz = CP * CY, CP * SY, SP
    local rx, ry, rz = SR * SP * CY - CR * SY, SR * SP * SY + CR * CY, -SR * CP
    local ux, uy, uz = -(CR * SP * CY + SR * SY), SR * CY - CR * SP * SY, CR * CP
    return fx, fy, fz, rx, ry, rz, ux, uy, uz
end

-- 行主序 4x4, clip = VP * (x, y, z, 1); FOV 度, aspect = 寬/高 (≥1), UE4 橫屏水平 FOV
local function build_view_projection(px, py, pz, pitch, yaw, roll, fov_deg, aspect, near, far)
    local fx, fy, fz, rx, ry, rz, ux, uy, uz = rotator_axes(pitch, yaw, roll)
    local xscl = 1.0 / math.tan(fov_deg * DEG * 0.5)
    local yscl = xscl * aspect
    local zscl = (far + near) / (far - near)
    local zoff = 2.0 * far * near / (far - near)
    local V = {
        { rx, ry, rz, -(rx * px + ry * py + rz * pz) },
        { ux, uy, uz, -(ux * px + uy * py + uz * pz) },
        { fx, fy, fz, -(fx * px + fy * py + fz * pz) },
        { 0.0, 0.0, 0.0, 1.0 },
    }
    local P = {
        { xscl, 0.0, 0.0, 0.0 },
        { 0.0, yscl, 0.0, 0.0 },
        { 0.0, 0.0, zscl, -zoff },
        { 0.0, 0.0, 1.0, 0.0 },
    }
    local VP = {}
    for i = 1, 4 do
        VP[i] = {}
        for j = 1, 4 do
            local acc = 0.0
            for k = 1, 4 do
                acc = acc + P[i][k] * V[k][j]
            end
            VP[i][j] = acc
        end
    end
    return VP
end

local function project(VP, x, y, z, sw, sh)
    local cx = VP[1][1] * x + VP[1][2] * y + VP[1][3] * z + VP[1][4]
    local cy = VP[2][1] * x + VP[2][2] * y + VP[2][3] * z + VP[2][4]
    local cw = VP[4][1] * x + VP[4][2] * y + VP[4][3] * z + VP[4][4]
    if cw == 0.0 then
        return nil, nil, nil
    end
    local nx = cx / cw
    local ny = cy / cw
    return (nx * 0.5 + 0.5) * sw, (0.5 - ny * 0.5) * sh, cw
end

-- 立方體 8 個角點: 局部 (±sx/2, ±sy/2, ±sz/2) 經 縮放→旋轉→平移 到世界座標再逐一投影
-- 成功回傳陣列 [1..8], 元素 {px, py, wx, wy, wz}; 任一角點在鏡後則整體回 nil
local BOX_EDGES = {
    { 1, 3 }, { 3, 7 }, { 7, 5 }, { 5, 1 },
    { 2, 4 }, { 4, 8 }, { 8, 6 }, { 6, 2 },
}
-- 15 姿勢骨連線: 頭-胸-盆 / 胸-肩-肘-腕 / 盆-大腿-膝-踝 (與 C++ 專案 pubg 模式一致)
local LIMB_PAIRS = {
    { 1, 2 }, { 2, 3 },
    { 2, 4 }, { 2, 5 }, { 4, 6 }, { 5, 7 }, { 6, 8 }, { 7, 9 },
    { 3, 10 }, { 3, 11 }, { 10, 12 }, { 11, 13 }, { 12, 14 }, { 13, 15 },
}
local CAP_STUB_CM = 15.0

local function cube_corners(VP, x, y, z, pitch, yaw, roll, sx, sy, sz, sw, sh)
    local hx, hy, hz = sx * 0.5, sy * 0.5, sz * 0.5
    local fx, fy, fz, rx, ry, rz, ux, uy, uz = rotator_axes(pitch, yaw, roll)
    local out = {}
    local k = 0
    for _, cx in ipairs({ -hx, hx }) do
        for _, cy in ipairs({ -hy, hy }) do
            for _, cz in ipairs({ -hz, hz }) do
                k = k + 1
                local wx = fx * cx + rx * cy + ux * cz + x
                local wy = fy * cx + ry * cy + uy * cz + y
                local wz = fz * cx + rz * cy + uz * cz + z
                local px, py, cw = project(VP, wx, wy, wz, sw, sh)
                if not px or cw <= 0.0 then
                    return nil
                end
                out[k] = { px = px, py = py, wx = wx, wy = wy, wz = wz }
            end
        end
    end
    return out
end

-- MinMax 求解立方體螢幕投影包圍盒; 與自瞄接獲判定同一份資料, 不重複投影
local function cube_minmax(corners)
    if not corners then
        return nil
    end
    local min_x, min_y, max_x, max_y = math.huge, math.huge, -math.huge, -math.huge
    for _, c in ipairs(corners) do
        if c.px < min_x then min_x = c.px end
        if c.py < min_y then min_y = c.py end
        if c.px > max_x then max_x = c.px end
        if c.py > max_y then max_y = c.py end
    end
    return min_x, min_y, max_x, max_y
end

local function draw_minmax_rect(min_x, min_y, max_x, max_y, r, g, b, a)
    theraohe.draw_line(min_x, min_y, max_x, min_y, r, g, b, a)
    theraohe.draw_line(max_x, min_y, max_x, max_y, r, g, b, a)
    theraohe.draw_line(max_x, max_y, min_x, max_y, r, g, b, a)
    theraohe.draw_line(min_x, max_y, min_x, min_y, r, g, b, a)
end

-- mesh 骨架 → 世界 2D 投影 (C++ 專案 pubg 模式解法):
--   mesh+0x210 inline ComponentToWorld FTransform → 4x4 矩陣;
--   bone 矩陣沿 c2w 旋轉部分旋轉 + 平移相加 (World = BoneLocal * c2w), 再投影;
--   僅投 15 姿勢骨 (頭/胸/盆/四肢), 索引用靜態表 (==68 → b68 表; 其餘 standard; 最大索引 ≥ num 即不認識, 不畫骨)
--   回傳 { num = 15, sxs, sys }; 槽位 1..15; 失敗 (讀不到 / 版本佈局不認識) 回 nil
local function mesh_bone_screen(VP, actor, x, y, z, rpitch, ryaw, rroll, sw, sh)
    local mesh = theraohe.read_u64(actor + MESH_OFF)
    if not mesh or mesh == 0 then
        return nil
    end
    -- c2w: 內聯 FTransform (Rotation quat + Translation + chunk + Scale3D) → 矩陣 (與 C++ TransformToMatrix 一致)
    local qx = theraohe.read_f32(mesh + COMPONENT_TO_WORLD_OFF)
    local qy = theraohe.read_f32(mesh + COMPONENT_TO_WORLD_OFF + 4)
    local qz = theraohe.read_f32(mesh + COMPONENT_TO_WORLD_OFF + 8)
    local qw = theraohe.read_f32(mesh + COMPONENT_TO_WORLD_OFF + 12)
    local tx = theraohe.read_f32(mesh + COMPONENT_TO_WORLD_OFF + 16)
    local ty = theraohe.read_f32(mesh + COMPONENT_TO_WORLD_OFF + 20)
    local tz = theraohe.read_f32(mesh + COMPONENT_TO_WORLD_OFF + 24)
    local sx = theraohe.read_f32(mesh + COMPONENT_TO_WORLD_OFF + 32)
    local sy = theraohe.read_f32(mesh + COMPONENT_TO_WORLD_OFF + 36)
    local sz = theraohe.read_f32(mesh + COMPONENT_TO_WORLD_OFF + 40)
    if not (qx and qy and qz and qw and tx and ty and tz and sx and sy and sz) then
        return nil
    end
    local x2, y2, z2 = qx + qx, qy + qy, qz + qz
    local xx2, yy2, zz2 = qx * x2, qy * y2, qz * z2
    local yz2, wx2 = qy * z2, qw * x2
    local xy2, wz2 = qx * y2, qw * z2
    local xz2, wy2 = qx * z2, qw * y2
    -- c2w 旋轉部分 (含 Scale3D), 平移
    local c11, c12, c13 = (1 - (yy2 + zz2)) * sx, (xy2 + wz2) * sx, (xz2 - wy2) * sx
    local c21, c22, c23 = (xy2 - wz2) * sy, (1 - (xx2 + zz2)) * sy, (yz2 + wx2) * sy
    local c31, c32, c33 = (xz2 + wy2) * sz, (yz2 - wx2) * sz, (1 - (xx2 + yy2)) * sz
    -- bone component-space: mesh+0xA30 TArray; 實際起點 Data + 0x30 (C++ 解法)
    local bone_data = (theraohe.read_u64(mesh + CACHED_COMPONENT_SPACE_OFF) or 0) + BONES_DATA_BIAS
    local num = theraohe.read_u32(mesh + CACHED_COMPONENT_SPACE_OFF + 8)
    if bone_data == 0 or not num or num < 1 or num > 256 then
        return nil
    end
    local indices = (num == 68) and BONE_TABLE_68 or BONE_TABLE_STANDARD
    local mx = 0
    for _, idx in ipairs(indices) do
        if idx > mx then
            mx = idx
        end
    end
    if mx >= num then
        -- 骨架版本未認識 (C++ pubg 模式只有 68/standard 兩種佈局)
        return nil
    end
    local sxs, sys, t = {}, {}, {}
    for i, idx in ipairs(indices) do
        local base = bone_data + idx * FTRANSFORM_SIZE
        local btx = theraohe.read_f32(base + 16)
        local bty = theraohe.read_f32(base + 20)
        local btz = theraohe.read_f32(base + 24)
        if btx and bty and btz then
            local wx = btx * c11 + bty * c21 + btz * c31 + tx
            local wy = btx * c12 + bty * c22 + btz * c32 + ty
            local wz = btx * c13 + bty * c23 + btz * c33 + tz
            if i == 1 then
                wz = wz + 7.0 -- 頭骨: C++ 專案脖子長 7cm
            end
            t[i] = { wx = wx, wy = wy, wz = wz }
            local px, py, cw = project(VP, wx, wy, wz, sw, sh)
            if px and cw > 0.0 then
                sxs[i] = px
                sys[i] = py
            end
        end
    end
    return { num = #indices, sxs = sxs, sys = sys, t = t }
end
-- 相機裝配: POV 位置/旋轉/FOV 同源, 見檔頭; 回傳 nil 表示本幀無法投影 (早退)
local function setup_camera()
    local onself = resolve_first(ONSELF_PATTERN)
    if not onself then
        return nil
    end
    local ctrl = resolve_first(CTRL_PATTERN)
    if not ctrl then
        return nil
    end
    local pcm = theraohe.read_u64(ctrl + 0x548)
    if not pcm or pcm == 0 then
        return nil
    end
    local px = theraohe.read_f32(pcm + 0x530)
    local py = theraohe.read_f32(pcm + 0x534)
    local pz = theraohe.read_f32(pcm + 0x538)
    local pitch = theraohe.read_f32(pcm + 0x548)
    local yaw = theraohe.read_f32(pcm + 0x54c)
    local roll = theraohe.read_f32(pcm + 0x550)
    -- 實際渲染 FOV 在 CameraCache POV 內 (pcm+0x554): 腰射 80.0, 開鏡 lerp 到武器倍率;
    -- ctrl+0x2B14 只是基礎 FOV, 開鏡不會變; 讀到異常值時退回基礎 FOV
    local fov = theraohe.read_f32(pcm + 0x554)
    if not fov or fov < 1.0 or fov > 170.0 then
        fov = theraohe.read_f32(ctrl + 0x2b14)
    end
    if not (px and py and pz and pitch and yaw and roll and fov) then
        return nil
    end
    local sw, sh = theraohe.screen_size()
    if not sw then
        return nil
    end
    local w, h = sw, sh
    if h > w then
        w, h = h, w
    end
    local aspect = w / h
    local near, far = 10.0, 1000000.0
    local VP = build_view_projection(px, py, pz, pitch, yaw, roll, fov, aspect, near, far)
    local my_team = theraohe.read_u32(onself + TEAM_ID_OFF)
    return {
        VP = VP, onself = onself, sw = sw, sh = sh,
        px = px, py = py, pz = pz,
        pitch = pitch, yaw = yaw, roll = roll, fov = fov,
        near = near, far = far, aspect = aspect,
        my_team = my_team,
    }
end

-- 走訪 Actors, 過濾出存活敵人, 只記錄 root 投影級資訊 (actor / root 投影 / 根世界座標)
-- 骨架與立方體角點是重成本: 只在需要時用 enemy_detail 對「候選」單個目標計算
-- 回傳 (enemies, actors_num); 失敗回傳 (nil, 錯誤碼) — 與舊 enemy_lines 一致
local function scan_enemy_roots(cam)
    local VP, onself, my_team, sw, sh = cam.VP, cam.onself, cam.my_team, cam.sw, cam.sh
    local gworld = resolve_first(GWORLD_PATTERN)
    if not gworld then
        return nil, -1
    end
    local level = theraohe.read_u64(gworld + 0x30)
    if not level then
        return nil, -2
    end
    local arr = theraohe.read_u64(level + 0xa0)
    local num = theraohe.read_u32(level + 0xa8)
    if not arr or not num then
        return nil, -3
    end
    if num > 4096 then
        return nil, num
    end
    local enemies = {}
    for i = 0, num - 1 do
        local actor = theraohe.read_u64(arr + i * 8)
        if actor and actor ~= 0 and actor ~= onself then
            -- 玩家 pawn: 專屬常量標記, 比 vtable 相等判斷穩妥 (不同載具/補給類不會誤入)
            local marker = theraohe.read_f32(actor + PLAYER_MARK_OFF)
            if marker == PLAYER_MARK then
                local state = theraohe.read_u32(actor + STATE_OFF)
                local team = theraohe.read_u32(actor + TEAM_ID_OFF)
                local is_bot = is_bot_flag(actor)
                -- TeamID<=0 多為無隊伍的 AI 佔位/載具幽靈, 跳過
                if not is_dead_state(state) and team and my_team and team ~= my_team and team > 0 then
                    local root = theraohe.read_u64(actor + 0x208)
                    if root and root ~= 0 then
                        local x = theraohe.read_f32(root + 0x1e4)
                        local y = theraohe.read_f32(root + 0x1e8)
                        local z = theraohe.read_f32(root + 0x1ec)
                        if x and y and z then
                            local rpitch = theraohe.read_f32(root + RELATIVE_ROTATION_OFF) or 0.0
                            local ryaw = theraohe.read_f32(root + RELATIVE_ROTATION_OFF + 4) or 0.0
                            local rroll = theraohe.read_f32(root + RELATIVE_ROTATION_OFF + 8) or 0.0
                            local sx, sy, cw = project(VP, x, y, z, sw, sh)
                            if not cw or cw <= 0.0 then
                                sx, sy = nil, nil
                            end
                            enemies[#enemies + 1] = {
                                actor = actor,
                                x = x, y = y, z = z,
                                sx = sx,
                                sy = sy,
                                is_bot = is_bot,
                                team = team,
                                rpitch = rpitch, ryaw = ryaw, rroll = rroll,
                            }
                        end
                    end
                end
            end
        end
    end
    return enemies, num
end

-- 詳細計算: 立方體角點 MinMax 包圍盒 + 骨架 (僅 ESP 連線與自瞄骨骼點需要);
-- 直接在 e 上附加 corners/min_x/min_y/max_x/max_y/bones/hb_x/hb_y 後回傳
local function enemy_detail(cam, e)
    local VP, sw, sh = cam.VP, cam.sw, cam.sh
    local bones = mesh_bone_screen(VP, e.actor, e.x, e.y, e.z, e.rpitch, e.ryaw, e.rroll, sw, sh)
    -- 立方體對齊當前姿勢實際身高: 頭骨 → 較低踝骨跨度 (+14cm 裕量); 無骨資料時退回 2m 固定
    local cz, sz = e.z, 200.0
    if bones and bones.t then
        local head = bones.t[1]
        local a14, a15 = bones.t[14], bones.t[15]
        local bot_z = math.huge
        if a14 then bot_z = math.min(bot_z, a14.wz) end
        if a15 then bot_z = math.min(bot_z, a15.wz) end
        if head and bot_z < math.huge and head.wz > bot_z then
            local top_z = head.wz + 15.0 -- 頭骨偏脖子位, 頭頂再加 15cm
            sz = math.max(top_z - bot_z, 50.0) -- 高度下限 50cm
            cz = (top_z + bot_z) * 0.5
        end
    end
    local corners = cube_corners(VP, e.x, e.y, cz, e.rpitch, e.ryaw, e.rroll, 60.0, 60.0, sz, sw, sh)
    local min_x, min_y, max_x, max_y = cube_minmax(corners)
    local ax, ay
    if e.sx then
        ax, ay = project(VP, e.x, e.y, cz + sz * 0.5, sw, sh)
    end
    e.ax, e.ay = ax, ay
    local hb = ((AimbotBone or -1) + 1)
    local hb_x, hb_y, hb_w
    if bones and bones.sxs[hb] then
        hb_x, hb_y = bones.sxs[hb], bones.sys[hb]
    end
    if bones and bones.t and bones.t[hb] then
        hb_w = { x = bones.t[hb].wx, y = bones.t[hb].wy, z = bones.t[hb].wz }
    end
    e.corners = corners
    e.min_x, e.min_y, e.max_x, e.max_y = min_x, min_y, max_x, max_y
    e.bones = bones
    e.hb_x, e.hb_y, e.hb_w = hb_x, hb_y, hb_w
    return e
end

-- 完整走訪: roots + 全部敵人詳細計算 (ESP 專用, 畫所有可見敵人)
local function collect_enemies(cam)
    local roots, num = scan_enemy_roots(cam)
    if not roots then
        return nil, num
    end
    local out = {}
    for i = 1, #roots do
        out[i] = enemy_detail(cam, roots[i])
    end
    return out, num
end

-- ESP 渲染: 天線線 / 立方體 / 骨骼連線 (高亮骨橙色); VP 供蓋角短柱重投影用
local function esp_render(VP, enemies, sw, sh)
    local hr, hg, hbh = 255, 165, 0
    local hb = ((AimbotBone or -1) + 1)
    for _, e in ipairs(enemies) do
        -- 人機 (IsBot) 畫白色; 真人畫紅色
        local cr, cg, cb = 255, 0, 0
        if e.is_bot then
            cr, cg, cb = 255, 255, 255
        end
        if e.ax then
            theraohe.draw_line(sw * 0.5, 0, e.ax, e.ay, cr, cg, cb, 255)
        end
        local corners = e.corners
        if corners then
            for _, ed in ipairs(BOX_EDGES) do
                theraohe.draw_line(corners[ed[1]].px, corners[ed[1]].py, corners[ed[2]].px, corners[ed[2]].py, cr, cg, cb, 255)
            end
            -- 蓋角短柱: 需要世界 up 基底; 與舊 draw_box 等價
            local fx, fy, fz, rx, ry, rz, ux, uy, uz = rotator_axes(e.rpitch, e.ryaw, e.rroll)
            for _, pair in ipairs({ { 1, 3, 5, 7, 1.0 }, { 2, 4, 6, 8, -1.0 } }) do
                local sign = pair[5]
                for k = 1, 4 do
                    local ci = pair[k]
                    local ex = corners[ci].wx + ux * CAP_STUB_CM * sign
                    local ey = corners[ci].wy + uy * CAP_STUB_CM * sign
                    local ez = corners[ci].wz + uz * CAP_STUB_CM * sign
                    local px, py, cw = project(VP, ex, ey, ez, sw, sh)
                    if px and cw > 0.0 then
                        theraohe.draw_line(corners[ci].px, corners[ci].py, px, py, cr, cg, cb, 255)
                    end
                end
            end
        end
        if e.bones then
            local b = e.bones
            for _, l in ipairs(LIMB_PAIRS) do
                local a, c = l[1], l[2]
                if b.sxs[a] and b.sxs[c] then
                    if a == hb or c == hb then
                        theraohe.draw_line(b.sxs[a], b.sys[a], b.sxs[c], b.sys[c], hr, hg, hbh, 170)
                    else
                        theraohe.draw_line(b.sxs[a], b.sys[a], b.sxs[c], b.sys[c], cr, cg, cb, 170)
                    end
                end
            end
        end
    end
end

-- 抬起自瞄手指: 任何路徑 (放手 / 未獲取 / 無目標) 都走此處
local function aim_release()
    if aim_finger_down and aim_x and aim_y then
        theraohe.touch_up(aim_x, aim_y)
    end
    aim_finger_down = false
    aim_x, aim_y = nil, nil
    aim_target = nil
    aim_requested = false
end

-- 整個 Tick 沒有 Aimbot_Pressing 呼叫 (使用者已放手) → 下一幀自動抬手; aim 模組卸下
local function aim_shortcut_down()
    return ticks <= aim_last_call + 1
end

local function aim_release_if_idle()
    if not aim_shortcut_down() then
        aim_requested = false
    end
    if aim_finger_down and not aim_shortcut_down() then
        aim_release()
    end
end

-- FullyAutoTrigger (slot 8): 同一組 按下→計時器滿間隙→鬆手→下次請求→重按 循環
-- 兩種觸發源共用本循環, 依優先序取第一個有效落點 (Aimbot 捷徑 > AutoTrigger):
--   A 捷徑按下且「FullyAutoTrigger」on → 落點 = 按壓位置
--   B 否則 AutoTrigger on 且目標對準 Forward 軸線 → 落點 = 最後 Aimbot_Pressing 落點
-- 都無 → 鬆手
local FIRE_SLOT = 8

local function fire_tap_release()
    if fire_finger_down then
        theraohe.touch_up(fire_x, fire_y, FIRE_SLOT)
    end
    fire_finger_down = false
end

-- Aimbot 捷徑開火: 捷徑按下且「FullyAutoTrigger」on → 落點 = 按壓位置
local function trigger_aim_fire()
    if aim_shortcut_down() and FullyAutoTrigger == true
        and aim_press_x and aim_press_y then
        return aim_press_x, aim_press_y
    end
end

-- AutoTrigger: Aimbot 未按下本 Tick, 且目標高亮骨世界座標點對準相機 Forward 軸線 → 落點 = 最後按壓位置
-- 根點 (root z = 腳底) 會帶入 ~150cm 垂直差, 故取骨骼點 (頭骨 +7cm 脖子位)
local function trigger_autotrigger(cam, nearest)
    if aim_shortcut_down() or not autotrigger_on then return end
    if not (aim_press_x and aim_press_y) then return end
    if not (nearest and nearest.hb_w) then return end
    local fx2, fy2, fz2 = rotator_axes(cam.pitch, cam.yaw, cam.roll)
    local dx, dy, dz = nearest.hb_w.x - cam.px, nearest.hb_w.y - cam.py, nearest.hb_w.z - cam.pz
    local fw = dx * fx2 + dy * fy2 + dz * fz2
    if fw <= 0.0 then return end
    local ux, uy, uz = dy * fz2 - dz * fy2, dz * fx2 - dx * fz2, dx * fy2 - dy * fx2
    local thr = tonumber(AutoTriggerThreshold) or 30
    if math.sqrt(ux * ux + uy * uy + uz * uz) <= thr then
        return aim_press_x, aim_press_y
    end
end

-- 依優先序回傳第一個有效落點 (Aimbot 捷徑 > AutoTrigger); 無落點 → 無回傳值
local function fire_requested(cam, nearest)
    local x, y = trigger_aim_fire()
    if x then return x, y end
    return trigger_autotrigger(cam, nearest)
end

local function fire_cycle(req_x, req_y)
    if req_x and req_y then
        if not fire_finger_down then
            fire_x, fire_y = req_x, req_y
            theraohe.touch_down(fire_x, fire_y, FIRE_SLOT)
            fire_finger_down = true
            fire_down_at = theraohe.now()
            return
        end
        local gap = tonumber(FireGap) or 0.05
        if theraohe.now() - fire_down_at >= gap then
            fire_tap_release()
        end
    else
        fire_tap_release()
    end
end

-- Tick 內的自瞄模組: 短捷徑只是「影響器」, 只決定本模組是否上班
-- enemies 直接複用 ESP 全詳細掃描結果 — 不重複走訪; 僅一個候選做詳細滑動
-- ShowAimbotRect / ShowCenterCross 是全局調試信息: 未按下自瞄鈕也顯示候選矩形與中心十字
-- AimbotRect 兩個語義: 黃框 = 可以接獲 (中心落在包圍盒); 紅框 = 正在追蹤快取目標 (脫框也畫)
local function aim_tick(cam, enemies)
    local sw, sh = cam.sw, cam.sh
    local cx, cy = sw * 0.5, sh * 0.5
    local nearest, nearest_d2
    for _, e in ipairs(enemies) do
        if e.sx then
            local dx, dy = e.sx - cx, e.sy - cy
            local d2 = dx * dx + dy * dy
            if not nearest_d2 or d2 < nearest_d2 then
                nearest, nearest_d2 = e, d2
            end
        end
    end
    if (ShowCenterCross) then
        theraohe.draw_line(cx - 12.0, cy, cx + 12.0, cy, 0, 255, 0, 240)
        theraohe.draw_line(cx, cy - 12.0, cx, cy + 12.0, 0, 255, 0, 240)
    end
    if ShowAimbotRect and nearest and nearest.min_x then
        local cached = aim_target ~= nil and nearest.actor == aim_target
        local in_box = cx >= nearest.min_x and cx <= nearest.max_x
            and cy >= nearest.min_y and cy <= nearest.max_y
        if cached then
            -- 已鎖定: 快取目標 (正在自瞄, 脫框也顯示), 畫紅框
            draw_minmax_rect(nearest.min_x, nearest.min_y, nearest.max_x, nearest.max_y, 255, 60, 0, 220)
        elseif in_box then
            -- 可接獲: 中心在盒內, 畫黃框
            draw_minmax_rect(nearest.min_x, nearest.min_y, nearest.max_x, nearest.max_y, 255, 255, 0, 220)
        end
    end
    -- FullyAutoTrigger / AutoTrigger (Aimbot 捷徑優先): 詳見 fire block 註釋
    fire_cycle(fire_requested(cam, nearest))
    -- 實際自瞄邏輯只在短捷徑積極按壓時執行
    if not aim_requested then
        return
    end
    if not nearest then
        aim_release()
        return
    end
    -- 最近敵人 == 已快取目標 → 直接追蹤, 不論位置
    local tracked = aim_target ~= nil and nearest.actor == aim_target
    if not tracked then
        -- 不是已快取目標: 只有螢幕中心落在包圍盒內才接獲
        local e = nearest
        if e.min_x and cx >= e.min_x and cx <= e.max_x
            and cy >= e.min_y and cy <= e.max_y then
            aim_target = e.actor
            tracked = true
        end
    end
    if not tracked then
        -- 既不是快取目標, 螢幕中心也不在其包圍盒 → 抬手 (無視本敵人)
        aim_release()
        return
    end
    -- 滑動: 目標骨骼 2D - 螢幕中心, 乘倍率; 無骨骼用根錨
    local tx = nearest.hb_x or nearest.sx
    local ty = nearest.hb_y or nearest.sy
    local mul_x = (AimbotMultiplierX or 1.0)
    local mul_y = (AimbotMultiplierY or 1.0)
    local dx = (tx - cx) * mul_x
    local dy = (ty - cy) * mul_y
    local max_drag = AimbotMaxDrag or 0
    if max_drag > 0 then
        local mag = math.sqrt(dx * dx + dy * dy)
        if mag > max_drag then
            print(string.format("[rodin] 觸碰滑動 %.0f px > %d px, 忽略本幀", mag, max_drag))
            return
        end
    end
    if not aim_finger_down then
        aim_finger_down = true
        aim_x = math.min(math.max((AimbotStartX or 2000), 0.0), sw - 1)
        aim_y = math.min(math.max((AimbotStartY or 200), 0.0), sh - 1)
        theraohe.touch_down(aim_x, aim_y)
    end
    aim_x = math.min(math.max(aim_x + dx, 0.0), sw - 1)
    aim_y = math.min(math.max(aim_y + dy, 0.0), sh - 1)
    theraohe.touch_move(aim_x, aim_y)
end

function Tick()
    aim_release_if_idle()
    ticks = ticks + 1
    local tick = ticks
    local verbose = tick % 120 == 1
    local cam = setup_camera()
    if not cam then
        if verbose then
            print("相機鏈失敗")
        end
        return
    end
    local sw, sh = cam.sw, cam.sh
    local enemies, anum = collect_enemies(cam)
    if not enemies then
        if verbose then
            print("敵人列舉失敗", anum)
        end
        return
    end
    local real, bot = 0, 0
    for _, e in ipairs(enemies) do
        if e.is_bot then
            bot = bot + 1
        else
            real = real + 1
        end
    end
    -- 人數統計 (永遠渲染, 0 也畫): 0 灰、>0 彩 (機器人綠 真人紅); 重要性 Heading
    local cx = sw * 0.5
    local bot_col = bot > 0 and { 0, 255, 0 } or { 255, 255, 255 }
    local real_col = real > 0 and { 255, 0, 0 } or { 255, 255, 255 }
    theraohe.draw_text(cx, 100, 1, 0, string.format("機器人:%d", bot), bot_col[1], bot_col[2], bot_col[3], nil, 2)
    theraohe.draw_text(cx, 100, 0, 0, string.format("真人:%d", real), real_col[1], real_col[2], real_col[3], nil, 2)
    -- Yaw 跟隨雷達: 田字正方框(白,淡) + 十字格 + 敵人 '○' + 面向線 (IntToRGB(team))
    local radar_size = RadarSize; local radar_range = RadarRange
    if radar_size and radar_range and radar_size > 0 and radar_range > 0 then
        local cx_r, cy_r = (RadarPosX or 800), (RadarPosY or 350)
        local half = radar_size * 0.5
        local range_cm = radar_range * 100.0
        local k = radar_size / range_cm
        theraohe.draw_line(cx_r - half, cy_r - half, cx_r + half, cy_r - half, 255, 255, 255, 110)
        theraohe.draw_line(cx_r + half, cy_r - half, cx_r + half, cy_r + half, 255, 255, 255, 110)
        theraohe.draw_line(cx_r + half, cy_r + half, cx_r - half, cy_r + half, 255, 255, 255, 110)
        theraohe.draw_line(cx_r - half, cy_r + half, cx_r - half, cy_r - half, 255, 255, 255, 110)
        -- 十字分隔, 田字 4 格
        theraohe.draw_line(cx_r - half, cy_r, cx_r + half, cy_r, 255, 255, 255, 110)
        theraohe.draw_line(cx_r, cy_r - half, cx_r, cy_r + half, 255, 255, 255, 110)
        -- 正前方 = 相機 POV Yaw (視角朝向); 敵人面向線用 actor root ryaw
        -- 主角 root z; 讀不到退回相機 POV z
        local own_root = theraohe.read_u64(cam.onself + 0x208)
        local own_z = (own_root and theraohe.read_f32(own_root + 0x1ec)) or cam.pz
        local yaw = cam.yaw * DEG
        local cyw, syw = math.cos(yaw), math.sin(yaw)
        local fwd_x, fwd_y = cyw, syw
        local right_x, right_y = -syw, cyw
        for _, e in ipairs(enemies) do
            local team = e.team or 1
            local tr, tg, tb = int_to_rgb(team)
            local dx, dy = e.x - cam.px, e.y - cam.py
            local half_range = range_cm * 0.5
            local f = dx * fwd_x + dy * fwd_y
            local s = dx * right_x + dy * right_y
            -- 超出範圍 → 映射到邊界上 (沿原方向縮放到方形邊緣)
            if math.abs(f) > half_range or math.abs(s) > half_range then
                local scale = math.min(half_range / math.max(math.abs(f), 1e-9), half_range / math.max(math.abs(s), 1e-9))
                f = f * scale
                s = s * scale
            end
            local px = cx_r + s * k
            local py = cy_r - f * k
            theraohe.draw_text(px, py, 0.5, 0.5, "○", tr, tg, tb, nil, 2)
            -- 高度差徽章: 敵高於/低於主角 → ▲(淺紅)/▼(淺藍) 繪於 ○ 上方; |Δz|<50cm 不畫
            local dz = e.z - own_z
            if dz >= 50.0 then
                theraohe.draw_text(px, py, 0.5, 0.5, "▲", 255, 0, 0, nil, 1)
            elseif dz <= -50.0 then
                theraohe.draw_text(px, py, 0.5, 0.5, "▼", 0, 0, 255, nil, 1)
            end
            local eyaw = e.ryaw * DEG
            local efx, efy = math.cos(eyaw), math.sin(eyaw)
            local es = efx * right_x + efy * right_y
            local ef = -(efx * fwd_x + efy * fwd_y)
            local m = math.sqrt(es * es + ef * ef)
            if m > 1e-6 then
                local ivec = 30.0 / m
                theraohe.draw_line(px, py, px + es * ivec, py + ef * ivec, tr, tg, tb, 255)
            end
        end
    end
    esp_render(cam.VP, enemies, sw, sh)
    aim_tick(cam, enemies)

    if not verbose then
        return
    end
    print(string.format("actors num=%d drawn=%d", anum, #enemies))
    print(string.format(
        "cam pos %.2f %.2f %.2f rot(deg) %.4f %.4f %.4f fov %.2f near %.1f far %.1f screen %dx%d aspect %.6f",
        cam.px, cam.py, cam.pz, cam.pitch, cam.yaw, cam.roll, cam.fov, cam.near, cam.far, sw, sh, cam.aspect))
    local VP = cam.VP
    for i = 1, 4 do
        print(string.format("VP row%d = %.6f %.6f %.6f %.6f",
            i - 1, VP[i][1], VP[i][2], VP[i][3], VP[i][4]))
    end

    -- 校驗: 主角 RootComponent 相對位置 ≈ 世界座標, 投影應落在螢幕偏下中央附近
    local root = theraohe.read_u64(cam.onself + 0x208)
    if root then
        local rx = theraohe.read_f32(root + 0x1e4)
        local ry = theraohe.read_f32(root + 0x1e8)
        local rz = theraohe.read_f32(root + 0x1ec)
        if rx and ry and rz then
            local sx, sy, cw = project(VP, rx, ry, rz, sw, sh)
            if sx then
                print(string.format(
                    "pad root %.2f %.2f %.2f -> screen (%.1f, %.1f), w=%.2f",
                    rx, ry, rz, sx, sy, cw))
            else
                print("pad 投影失敗 (cw=0)")
            end
        end
    end
end

-- 短捷徑事件: 與開火鍵重疊之"影響器"; 只決定 Tick 內的自瞄模組是否上班,
-- 一切自瞄邏輯 (走訪/判定/滑動/抬手) 都在 Tick 的 aim 模組內處理, 不在此函式裡跑
-- x,y = 捷徑按下時手指的屏幕物理像素; 省略則落點清空, FullyAutoTrigger/AutoTrigger 讓位
Aimbot_Pressing = function(x, y)
    aim_last_call = ticks
    aim_requested = true
    -- 快取: 每次有效 xy 呼叫更新 (寄信機制: 此即捷徑按下時的位置, 供 AutoTrigger 複用)
    aim_press_x, aim_press_y = x, y
end

-- AutoTrigger開關 (捷徑為開關式按鈕: 點擊翻轉並回傳 Bool, 回傳值自動成為按鈕狀態)
-- 啓用後, Aimbot 沒有按下時, 瞄準目標對準相機 Forward 軸線即在最後 Aimbot_Pressing 落點進入連點循環;
-- 落點尚未有過 Aimbot_Pressing 則整個不生效, 須先按一次捷徑
AutoTrigger = function()
    autotrigger_on = not autotrigger_on
    if autotrigger_on and not (aim_press_x and aim_press_y) then
        print("[rodin] AutoTrigger 已開啓, 尚無 Aimbot_Pressing 落點 — 先按一下捷徑")
    end
    return autotrigger_on
end
