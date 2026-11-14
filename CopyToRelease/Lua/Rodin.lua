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
--   所有角色 (玩家與 AI 靶人) 都是同一個 STExtraBaseCharacter 類, 以 onself 的 vtable 識別;
--   再讀 RootComponent +0x208 -> RelativeLocation +0x1E4 得世界座標
-- 自身跳過; 死亡 (bDead +0xE8C) 與隊友 (TeamID +0x9A8) 跳過; 人機 (IsBot +0xA69) 畫灰線, 真人畫紅線
--
-- Aimbot (aim 模組): 配置為捷徑後與遊戲開火鍵重疊; 短捷徑只是「影響器」, 實際自瞄邏輯都在 Tick 內處理
--   走訪所有可見敵人 → 取螢幕中心距離最近者 → 已快取目標: 直接追蹤滑動
--   否則: 立方體螢幕投影 MinMax 包圍盒, 中心點落入才接獲為目標並追蹤
--   (enemies 全詳細計算只算一次, ESP 與 aim 模組共用)
--   接獲/失去目標的判定只在「非快取目標」時進行; 快取目標只要使用者不鬆開捷徑就一直自瞄
--   倍率 Y=0 = 只水平 (用戶自行壓槍), X 依舊照常工作
--   自瞄手指固定 slot 9, 與使用者手指不競爭; 使用者放手 (整個 Tick 缺席) 時下一幀抬手指
local module_candidates = { "libue4.so", "UE4", "Unreal", "Game", "PUBGM" }

local CTRL_PATTERN = '[[[[[["%s"+0xFC70A70]+0x810]+0x78]+0x38]+0x78]+0x30]'
local ONSELF_PATTERN = '[[[[[[["%s"+0xFC70A70]+0x810]+0x78]+0x38]+0x78]+0x30]+0x29F8]'
local GWORLD_PATTERN = '[[["%s"+0xFC70A70]+0x810]+0x78]'

local TEAM_ID_OFF = 0x9a8
local B_DEAD_OFF = 0xe8c
local IS_BOT_OFF = 0xa69
-- RootComponent 內: RelativeLocation +0x1E4, RelativeRotation +0x1F0 (FRotator, 角度)
local RELATIVE_ROTATION_OFF = 0x1f0
-- SkeletalMeshComponent: STCharacter -> Mesh +0x510; TArray<FTransform> 偏移由本 build 實測 (tick 1 掃描確認 67 骨, MCS 頭 z 遞增)
local MESH_OFF = 0x510
local CACHED_COMPONENT_SPACE_OFF = 0xa30
local CACHED_BONE_SPACE_OFF = 0xc98
-- UE4 FTransform (FQuat 對齊 16B): Rotation 0x00(f32x4), Translation 0x10(f32x3), Scale3D 0x1C, SizeOf=48
local FTRANSFORM_SIZE = 48
local MAX_BONES = 2048
-- pubgm 人形骨架 MCS 局部 z 以腳為原點; 根膠囊中心則在髖部 (root z), 二者高度差寫死
local PUBGM_CAPSULE_HALF = 85.0
-- 骨鏈僅繪製斷點長度 >= 閾值的線段 (短骨如手指不顯示)
local BONE_MIN_LINK_CM = 10.0

-- 全域: 高亮骨骼索引, 橙線渲染; <0 = 不高亮。TheRaohe Lua 頁面可調 (公共整型變量); 6 = 自瞄骨頭
HighlightBone = 6
-- 自瞄手指與行為 (Lua 頁面公開, 物理像素座標)
-- 手指起手位: 該處有空白遊戲區域最佳; 會自動鉗位到螢幕內
AimbotStartX = 2000
AimbotStartY = 200
-- 滑動倍率 (螢幕中心-敵人位置差 * 倍率 = 滑動距離); 涵蓋速度與鏡頭靈敏度
-- Y=0 = 只水平瞄準 (自行壓槍), X 仍照常工作
AimbotMultiplierX = 0.05
AimbotMultiplierY = 0.0
-- 畫自瞄用立方體 MinMax 包圍盒 (黃框, 所有可見敵人); ShowCenterCross 畫螢幕中心十字 (綠)
ShowAimbotRect = true
ShowCenterCross = true

-- 自瞄內部狀態 (不反射)
local aim_finger_down = false   -- 自瞄手指是否已落下
local aim_x, aim_y = nil, nil   -- 自瞄手指當前位置; nil = 未落下
local aim_target = nil          -- 已快取自瞄目標 (actor ptr 整數)
local aim_last_call = -1        -- 最後一次 Aimbot_Pressing 的 tick 序號; 使用者放手時 => ticks 跳號
local aim_requested = false     -- 捷徑按住狀態 (影響器); 實際自瞄邏輯在 Tick 的 aim 模組執行

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

-- 小端 f32 解碼; o 為 0 基字節偏移
local function f32_at(s, o)
    local b1, b2, b3, b4 = string.byte(s, o + 1, o + 4)
    local bits = ((b4 << 24) | (b3 << 16) | (b2 << 8) | b1) & 0xFFFFFFFF
    local sign = (bits >> 31) ~= 0 and -1.0 or 1.0
    local exp = (bits >> 23) & 0xFF
    local mant = bits & 0x007FFFFF
    if exp == 0 then
        return sign * math.ldexp(mant, -126 - 23)
    elseif exp == 255 then
        return sign * math.huge
    end
    return sign * math.ldexp(1.0 + mant * 2.0 ^ -23, exp - 127)
end

-- 讀整段陣列 (4096 上限分塊), 失敗回 nil
local function read_chunks(addr, len)
    local parts = {}
    local off = 0
    while off < len do
        local chunk = math.min(len - off, 4096)
        local s = theraohe.read_bytes(addr + off, chunk)
        if not s or #s ~= chunk then
            return nil
        end
        parts[#parts + 1] = s
        off = off + chunk
    end
    return table.concat(parts)
end

-- TArray<FTransform> -> { quat(x,y,z,w), trans(x,y,z) }[1..num]
local function decode_transforms(addr, num)
    local blob = read_chunks(addr, num * FTRANSFORM_SIZE)
    if not blob then
        return nil
    end
    local out = {}
    for i = 1, num do
        local base = (i - 1) * FTRANSFORM_SIZE
        out[i] = {
            qx = f32_at(blob, base + 0),
            qy = f32_at(blob, base + 4),
            qz = f32_at(blob, base + 8),
            qw = f32_at(blob, base + 12),
            tx = f32_at(blob, base + 16),
            ty = f32_at(blob, base + 20),
            tz = f32_at(blob, base + 24),
        }
    end
    return out
end

-- 只解碼指定骨 (烘焙後用): 只取 translation, 跳過 quat/scale
local function decode_kept(addr, num, keep)
    local blob = read_chunks(addr, num * FTRANSFORM_SIZE)
    if not blob then
        return nil
    end
    local out = {}
    for i = 1, num do
        if keep[i] then
            local base = (i - 1) * FTRANSFORM_SIZE
            out[i] = {
                tx = f32_at(blob, base + 16),
                ty = f32_at(blob, base + 20),
                tz = f32_at(blob, base + 24),
            }
        end
    end
    return out
end

local function qmul(ax, ay, az, aw, bx, by, bz, bw)
    return aw * bx + ax * bw + ay * bz - az * by,
           aw * by - ax * bz + ay * bw + az * bx,
           aw * bz + ax * by - ay * bx + az * bw,
           aw * bw - ax * bx - ay * by - az * bz
end

local function qrot(qx, qy, qz, qw, vx, vy, vz)
    local tx = 2.0 * (qy * vz - qz * vy)
    local ty = 2.0 * (qz * vx - qx * vz)
    local tz = 2.0 * (qx * vy - qy * vx)
    return vx + qw * tx + qy * tz - qz * ty,
           vy + qw * ty + qz * tx - qx * tz,
           vz + qw * tz + qx * ty - qy * tx
end

local function qdot_abs(ax, ay, az, aw, bx, by, bz, bw)
    return math.abs(ax * bx + ay * by + az * bz + aw * bw)
end

-- 父骨還原 + 骨絡烘焙: 首次對該骨架 (num) 建一次
--   1. MCS[i] 應等於 MCS[p] * BS[i] (旋轉 Hamilton 積, 平移 p.t + p.rot*bs.t); 不匹配即放棄
--   2. 依當前姿勢斷點長度標記需要解碼/投影的骨骼 (>= BONE_MIN_LINK_CM): 兩端同記, 烘焙後
--      該 num 的後續幀直接跳過短骨鏈 (指/腕等), 不受運動時斷點長度<=閾值的影響
-- 不依賴骨骼名字/父索引佈局, PUBGM 骨架任意版本可重建; 按 num 緩存, 一局只算一次。
local skeleton_cache = {}
local function bake_skeleton(num, mcs, bs)
    local cached = skeleton_cache[num]
    if cached then
        return cached
    end
    local parents = {}
    for i = 2, num do
        local bi = bs[i]
        local qi = mcs[i]
        local best, best_d2
        for p = 1, i - 1 do
            local pi = mcs[p]
            local dx = bi.tx
            local dy = bi.ty
            local dz = bi.tz
            local rx, ry, rz = qrot(pi.qx, pi.qy, pi.qz, pi.qw, dx, dy, dz)
            local ddx = pi.tx + rx - qi.tx
            local ddy = pi.ty + ry - qi.ty
            local ddz = pi.tz + rz - qi.tz
            local d2 = ddx * ddx + ddy * ddy + ddz * ddz
            if d2 < 25.0 and (best_d2 == nil or d2 < best_d2) then
                local cx, cy, cz, cw = qmul(pi.qx, pi.qy, pi.qz, pi.qw, bi.qx, bi.qy, bi.qz, bi.qw)
                if qdot_abs(cx, cy, cz, cw, qi.qx, qi.qy, qi.qz, qi.qw) > 0.999 then
                    best, best_d2 = p, d2
                end
            end
        end
        parents[i] = best
    end
    local min2 = BONE_MIN_LINK_CM * BONE_MIN_LINK_CM
    local keep = {}
    for i = 2, num do
        local pi = parents[i]
        if pi then
            local dx = mcs[i].tx - mcs[pi].tx
            local dy = mcs[i].ty - mcs[pi].ty
            local dz = mcs[i].tz - mcs[pi].tz
            if dx * dx + dy * dy + dz * dz >= min2 then
                keep[i] = true
                keep[pi] = true
            end
        end
    end
    local out = { parents = parents, keep = keep }
    skeleton_cache[num] = out
    return out
end

-- mesh 骨架 → 世界 2D 投影: 組件空間骨位置先旋轉入人物局部座標系, 再投影
-- 回傳 { num, sxs, sys, parents, keep }; sxs/sys 被過濾的骨為 nil; 失敗或無骨回 nil
local function mesh_bone_screen(VP, actor, x, y, z, rpitch, ryaw, rroll, sw, sh)
    local mesh = theraohe.read_u64(actor + MESH_OFF)
    if not mesh or mesh == 0 then
        return nil
    end
    local mcs_data = theraohe.read_u64(mesh + CACHED_COMPONENT_SPACE_OFF)
    local num = theraohe.read_u32(mesh + CACHED_COMPONENT_SPACE_OFF + 8)
    if not mcs_data or mcs_data == 0 or not num or num < 2 or num > MAX_BONES then
        return nil
    end
    -- 骨絡烘焙 (父骨 + 需要解碼的骨): 首次需要 BS, 之後直接取緩存
    local skel = skeleton_cache[num]
    local mcs
    if skel then
        mcs = decode_kept(mcs_data, num, skel.keep)
    else
        local full = decode_transforms(mcs_data, num)
        if not full then
            return nil
        end
        local bs_data = theraohe.read_u64(mesh + CACHED_BONE_SPACE_OFF)
        local bs_num = theraohe.read_u32(mesh + CACHED_BONE_SPACE_OFF + 8)
        if bs_data and bs_data ~= 0 and bs_num == num then
            local bs = decode_transforms(bs_data, num)
            if bs then
                skel = bake_skeleton(num, full, bs)
            end
        end
        if not skel then
            return nil
        end
        mcs = full
    end
    if not mcs then
        return nil
    end
    -- mesh 本身相對根組件: RelativeLocation (+0x1E4), RelativeRotation (+0x1F0); mesh 父缺省為根
    local mpx = theraohe.read_f32(mesh + 0x1e4) or 0.0
    local mpy = theraohe.read_f32(mesh + 0x1e8) or 0.0
    local mpz = theraohe.read_f32(mesh + 0x1ec) or 0.0
    local mpitch = theraohe.read_f32(mesh + 0x1f0) or 0.0
    local myaw = theraohe.read_f32(mesh + 0x1f4) or 0.0
    local mroll = theraohe.read_f32(mesh + 0x1f8) or 0.0
    local fx, fy, fz, rx, ry, rz, ux, uy, uz = rotator_axes(rpitch, ryaw, rroll)
    local mfx, mfy, mfz, mrx, mry, mrz, mux, muy, muz = rotator_axes(mpitch, myaw, mroll)
    -- mesh 基底 (每列為 mesh 局部軸在世界上的方向)
    local ex = fx * mfx + rx * mfy + ux * mfz
    local ey = fy * mfx + ry * mfy + uy * mfz
    local ez = fz * mfx + rz * mfy + uz * mfz
    local eyx = fx * mrx + rx * mry + ux * mrz
    local eyy = fy * mrx + ry * mry + uy * mrz
    local eyz = fz * mrx + rz * mry + uz * mrz
    local ezx = fx * mux + rx * muy + ux * muz
    local ezy = fy * mux + ry * muy + uy * muz
    local ezz = fz * mux + rz * muy + uz * muz
    -- mesh 原點 (世界): 根位置 + 根基底 * mesh 相對位置, 再下移膠囊半高 (骨架腳原點 vs 根中心)
    local ox = x + fx * mpx + rx * mpy + ux * mpz
    local oy = y + fy * mpx + ry * mpy + uy * mpz
    local oz = z + fz * mpx + rz * mpy + uz * mpz - PUBGM_CAPSULE_HALF
    local sxs, sys, t = {}, {}, {}
    for i = 1, num do
        local bi = mcs[i]
        if bi then
            local wx = ex * bi.tx + eyx * bi.ty + ezx * bi.tz + ox
            local wy = ey * bi.tx + eyy * bi.ty + ezy * bi.tz + oy
            local wz = ez * bi.tx + eyz * bi.ty + ezz * bi.tz + oz
            local px, py, cw = project(VP, wx, wy, wz, sw, sh)
            if px and cw > 0.0 then
                sxs[i], sys[i] = px, py
                t[i] = { tx = bi.tx, ty = bi.ty, tz = bi.tz }
            end
        end
    end
    return { num = num, sxs = sxs, sys = sys, t = t, parents = skel.parents, keep = skel.keep }
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

-- 走訪 Actors, 過濾出存活敵人, 只記錄 root 投影級資訊 (actor / root 投影 / 根世界座標 / 天線投影)
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
    local own_vtbl = theraohe.read_u64(onself)
    if not own_vtbl then
        return nil, -4
    end
    local enemies = {}
    for i = 0, num - 1 do
        local actor = theraohe.read_u64(arr + i * 8)
        if actor and actor ~= 0 and actor ~= onself then
            local vtbl = theraohe.read_u64(actor)
            if vtbl == own_vtbl then
                local dead = theraohe.read_u8(actor + B_DEAD_OFF)
                local team = theraohe.read_u32(actor + TEAM_ID_OFF)
                local is_bot = theraohe.read_u8(actor + IS_BOT_OFF)
                if dead == 0 and team and my_team and team ~= my_team then
                    local root = theraohe.read_u64(actor + 0x208)
                    if root and root ~= 0 then
                        local x = theraohe.read_f32(root + 0x1e4)
                        local y = theraohe.read_f32(root + 0x1e8)
                        local z = theraohe.read_f32(root + 0x1ec)
                        if x and y and z then
                            local sx, sy, cw = project(VP, x, y, z, sw, sh)
                            if sx and cw > 0 then
                                local rpitch = theraohe.read_f32(root + RELATIVE_ROTATION_OFF) or 0.0
                                local ryaw = theraohe.read_f32(root + RELATIVE_ROTATION_OFF + 4) or 0.0
                                local rroll = theraohe.read_f32(root + RELATIVE_ROTATION_OFF + 8) or 0.0
                                -- 天線投影終點 (高 z+100)
                                local ax, ay = project(VP, x, y, z + 100.0, sw, sh)
                                enemies[#enemies + 1] = {
                                    actor = actor,
                                    x = x, y = y, z = z,
                                    sx = sx,
                                    sy = sy,
                                    is_bot = is_bot ~= nil and is_bot ~= 0,
                                    rpitch = rpitch, ryaw = ryaw, rroll = rroll,
                                    ax = ax, ay = ay,
                                }
                            end
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
    local corners = cube_corners(VP, e.x, e.y, e.z, e.rpitch, e.ryaw, e.rroll, 60.0, 60.0, 200.0, sw, sh)
    local min_x, min_y, max_x, max_y = cube_minmax(corners)
    local bones = mesh_bone_screen(VP, e.actor, e.x, e.y, e.z, e.rpitch, e.ryaw, e.rroll, sw, sh)
    local hb = (HighlightBone or -1) + 1
    local hb_x, hb_y
    if bones and bones.sxs[hb] then
        hb_x, hb_y = bones.sxs[hb], bones.sys[hb]
    end
    e.corners = corners
    e.min_x, e.min_y, e.max_x, e.max_y = min_x, min_y, max_x, max_y
    e.bones = bones
    e.hb_x, e.hb_y = hb_x, hb_y
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
            local min2 = BONE_MIN_LINK_CM * BONE_MIN_LINK_CM
            local hb = HighlightBone or -1
            for i = 2, b.num do
                local pi = b.parents[i]
                if pi and b.sxs[i] and b.sxs[pi] and b.t[i] and b.t[pi] then
                    local dx = b.t[i].tx - b.t[pi].tx
                    local dy = b.t[i].ty - b.t[pi].ty
                    local dz = b.t[i].tz - b.t[pi].tz
                    if dx * dx + dy * dy + dz * dz >= min2 then
                        if i - 1 == hb or pi - 1 == hb then
                            theraohe.draw_line(b.sxs[pi], b.sys[pi], b.sxs[i], b.sys[i], hr, hg, hbh, 170)
                        else
                            theraohe.draw_line(b.sxs[pi], b.sys[pi], b.sxs[i], b.sys[i], cr, cg, cb, 170)
                        end
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
local function aim_release_if_idle()
    if ticks > aim_last_call + 1 then
        aim_requested = false
    end
    if aim_finger_down and ticks > aim_last_call + 1 then
        aim_release()
    end
end

-- Tick 內的自瞄模組: 短捷徑只是「影響器」, 只決定本模組是否上班
-- enemies 直接複用 ESP 全詳細掃描結果 — 不重複走訪; 僅一個候選做詳細滑動
-- ShowAimbotRect / ShowCenterCross 是全局調試信息: 未按下自瞄鈕也顯示候選矩形與中心十字
-- AimbotRect 兩個語義: 黃框 = 可以接獲 (中心落在包圍盒); 綠框 = 正在追蹤快取目標 (脫框也畫)
local function aim_tick(cam, enemies)
    local sw, sh = cam.sw, cam.sh
    local cx, cy = sw * 0.5, sh * 0.5
    local nearest, nearest_d2
    for _, e in ipairs(enemies) do
        local dx, dy = e.sx - cx, e.sy - cy
        local d2 = dx * dx + dy * dy
        if not nearest_d2 or d2 < nearest_d2 then
            nearest, nearest_d2 = e, d2
        end
    end
    if ShowCenterCross then
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
    local mul_x = AimbotMultiplierX or 1.0
    local mul_y = AimbotMultiplierY or 1.0
    local dx = (tx - cx) * mul_x
    local dy = (ty - cy) * mul_y
    if not aim_finger_down then
        aim_finger_down = true
        aim_x = math.min(math.max(AimbotStartX, 0.0), sw - 1)
        aim_y = math.min(math.max(AimbotStartY, 0.0), sh - 1)
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
function Aimbot_Pressing()
    aim_last_call = ticks
    aim_requested = true
end
