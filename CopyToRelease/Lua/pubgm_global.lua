-- PUBG Mobile Global: 3D→2D ViewProjection 矩陣 + 敵人 ESP (螢幕中心頂 ↔ 敵人 z+100 線) 與 3D 方塊
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
local module_candidates = { "libue4.so", "UE4", "Unreal", "Game", "PUBGM" }

local CTRL_PATTERN = '[[[[[["%s"+0xFC70A70]+0x810]+0x78]+0x38]+0x78]+0x30]'
local ONSELF_PATTERN = '[[[[[[["%s"+0xFC70A70]+0x810]+0x78]+0x38]+0x78]+0x30]+0x29F8]'
local GWORLD_PATTERN = '[[["%s"+0xFC70A70]+0x810]+0x78]'

local TEAM_ID_OFF = 0x9a8
local B_DEAD_OFF = 0xe8c
local IS_BOT_OFF = 0xa69
-- RootComponent 內: RelativeLocation +0x1E4, RelativeRotation +0x1F0 (FRotator, 角度)
local RELATIVE_ROTATION_OFF = 0x1f0

local function resolve_first(pattern)
    for _, pat in ipairs(module_candidates) do
        local p = theraohe.resolve_chain(string.format(pattern, pat))
        if p then
            return p
        end
    end
    return nil
end

function init()
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

-- 3D 方塊 → 2D 線段: 局部 8 角點經 縮放→旋轉→平移 到世界座標, 再逐一投影, 畫 12 條棱。
-- 盒子以 (x,y,z) 為中心; pitch/yaw/roll 為世界座標下的 FRotator (角度); 尺寸與位置同單位 (cm)。
-- 任一角點在鏡後 (投影失敗) 整體放棄, 避免穿幫連線
local BOX_EDGES = {
    { 1, 3 }, { 3, 7 }, { 7, 5 }, { 5, 1 },
    { 2, 4 }, { 4, 8 }, { 8, 6 }, { 6, 2 },
    { 1, 2 }, { 3, 4 }, { 5, 6 }, { 7, 8 },
}

local function draw_box(VP, x, y, z, pitch, yaw, roll, sx, sy, sz, r, g, b, a, sw, sh)
    local hx, hy, hz = sx * 0.5, sy * 0.5, sz * 0.5
    local fx, fy, fz, rx, ry, rz, ux, uy, uz = rotator_axes(pitch, yaw, roll)
    local sxs, sys = {}, {}
    local idx = 0
    for _, cx in ipairs({ -hx, hx }) do
        for _, cy in ipairs({ -hy, hy }) do
            for _, cz in ipairs({ -hz, hz }) do
                idx = idx + 1
                local wx = fx * cx + rx * cy + ux * cz + x
                local wy = fy * cx + ry * cy + uy * cz + y
                local wz = fz * cx + rz * cy + uz * cz + z
                local px, py, cw = project(VP, wx, wy, wz, sw, sh)
                if not px or cw <= 0.0 then
                    return
                end
                sxs[idx], sys[idx] = px, py
            end
        end
    end
    for _, e in ipairs(BOX_EDGES) do
        theraohe.draw_line(sxs[e[1]], sys[e[1]], sxs[e[2]], sys[e[2]], r, g, b, a)
    end
end

-- 移動向量採樣: 與 30 幀前的採樣比對 (≈0.5s), 逐幀位移太短方向會抖; 採樣窗內沿用上次算好的方向,
-- 窗內位移 < 20cm 視為靜止 (站立/蹲著近乎 0), 靜止不畫
local MV_SAMPLE_FRAMES = 30
local MV_STILL_CM = 20.0
local enemy_hist = {}

local function movement_vector(actor, x, y, z, tick)
    local e = enemy_hist[actor]
    if not e then
        enemy_hist[actor] = { x = x, y = y, z = z, tick = tick, dx = 0.0, dy = 0.0, dz = 0.0 }
        return nil
    end
    if tick - e.tick >= MV_SAMPLE_FRAMES then
        e.dx, e.dy, e.dz = x - e.x, y - e.y, z - e.z
        e.x, e.y, e.z, e.tick = x, y, z, tick
    end
    local len = math.sqrt(e.dx * e.dx + e.dy * e.dy + e.dz * e.dz)
    if len < MV_STILL_CM then
        return nil
    end
    return e.dx / len, e.dy / len, e.dz / len
end

local function enemy_lines(VP, onself, my_team, sw, sh, tick)
    local gworld = resolve_first(GWORLD_PATTERN)
    if not gworld then
        return 0, -1
    end
    local level = theraohe.read_u64(gworld + 0x30)
    if not level then
        return 0, -2
    end
    local arr = theraohe.read_u64(level + 0xa0)
    local num = theraohe.read_u32(level + 0xa8)
    if not arr or not num then
        return 0, -3
    end
    if num > 4096 then
        return 0, num
    end
    local own_vtbl = theraohe.read_u64(onself)
    if not own_vtbl then
        return 0, -4
    end
    local drawn = 0
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
                            local mvx, mvy, mvz = movement_vector(actor, x, y, z, tick)
                            local sx, sy, cw = project(VP, x, y, z, sw, sh)
                            if sx and cw > 0 then
                                -- 人機 (IsBot) 畫白色; 真人畫紅色
                                local cr, cg, cb = 255, 0, 0
                                if is_bot and is_bot ~= 0 then
                                    cr, cg, cb = 255, 255, 255
                                end
                                -- 天線終點: 敵人所在 3D 點向上 +100cm, 投影後作為射線終點
                                local ax, ay = project(VP, x, y, z + 100.0, sw, sh)
                                if ax then
                                    theraohe.draw_line(sw / 2, 0, ax, ay, cr, cg, cb, 255)
                                end
                                local rpitch = theraohe.read_f32(root + RELATIVE_ROTATION_OFF) or 0.0
                                local ryaw = theraohe.read_f32(root + RELATIVE_ROTATION_OFF + 4) or 0.0
                                local rroll = theraohe.read_f32(root + RELATIVE_ROTATION_OFF + 8) or 0.0
                                draw_box(VP, x, y, z, rpitch, ryaw, rroll, 60.0, 60.0, 200.0, cr, cg, cb, 255, sw, sh)
                                -- 移動向量: 從角色中心指向移動方向, 定長 100cm; 靜止不畫
                                if mvx then
                                    local tx, ty, tcw = project(VP, x + mvx * 100.0, y + mvy * 100.0, z + mvz * 100.0, sw, sh)
                                    if tx and tcw > 0 then
                                        theraohe.draw_line(sx, sy, tx, ty, 255, 255, 0, 255)
                                    end
                                end
                                drawn = drawn + 1
                            end
                        end
                    end
                end
            end
        end
    end
    -- 清理已消失敵人的殘留採樣, 避免長時間遊戲下表無限膨脹
    if tick % 600 == 0 then
        for a, e in pairs(enemy_hist) do
            if tick - e.tick > 600 then
                enemy_hist[a] = nil
            end
        end
    end
    return drawn, num
end


local ticks = 0

function Tick()
    ticks = ticks + 1
    local tick = ticks
    local verbose = tick % 120 == 1
    local onself = resolve_first(ONSELF_PATTERN)
    if not onself then
        if verbose then
            print("[pubgm] onself 鏈失敗")
        end
        return
    end
    local ctrl = resolve_first(CTRL_PATTERN)
    if not ctrl then
        if verbose then
            print("[pubgm] ctrl 鏈失敗")
        end
        return
    end
    local pcm = theraohe.read_u64(ctrl + 0x548)
    if not pcm or pcm == 0 then
        if verbose then
            print("[pubgm] PlayerCameraManager 讀取失敗")
        end
        return
    end
    local px = theraohe.read_f32(pcm + 0x530)
    local py = theraohe.read_f32(pcm + 0x534)
    local pz = theraohe.read_f32(pcm + 0x538)
    -- 位置/旋轉/FOV 同抄一個 POV, 見檔頭註解
    local pitch = theraohe.read_f32(pcm + 0x548)
    local yaw = theraohe.read_f32(pcm + 0x54c)
    local roll = theraohe.read_f32(pcm + 0x550)
    -- 實際渲染 FOV 在 CameraCache POV 內 (pcm+0x554): 腰射 80.0, 開鏡 lerp 到武器倍率 (例如 3x = 26.6667);
    -- ctrl+0x2B14 只是基礎 FOV, 開鏡不會變; 讀到異常值時退回基礎 FOV
    local fov = theraohe.read_f32(pcm + 0x554)
    if not fov or fov < 1.0 or fov > 170.0 then
        fov = theraohe.read_f32(ctrl + 0x2b14)
    end
    if not (px and py and pz and pitch and yaw and roll and fov) then
        if verbose then
            print("[pubgm] 相機欄位讀取失敗")
        end
        return
    end
    local sw, sh = theraohe.screen_size()
    if not sw then
        if verbose then
            print("[pubgm] screen_size 查詢失敗")
        end
        return
    end
    local w, h = sw, sh
    if h > w then
        w, h = h, w
    end
    local aspect = w / h
    local near, far = 10.0, 1000000.0
    local VP = build_view_projection(px, py, pz, pitch, yaw, roll, fov, aspect, near, far)

    local my_team = theraohe.read_u32(onself + TEAM_ID_OFF)
    local esp, anum = enemy_lines(VP, onself, my_team, sw, sh, tick)

    if not verbose then
        return
    end
    print(string.format("[pubgm] actors num=%d drawn=%d", anum, esp))
    print(string.format(
        "[pubgm] cam pos %.2f %.2f %.2f rot(deg) %.4f %.4f %.4f fov %.2f near %.1f far %.1f screen %dx%d aspect %.6f",
        px, py, pz, pitch, yaw, roll, fov, near, far, sw, sh, aspect))
    for i = 1, 4 do
        print(string.format("[pubgm] VP row%d = %.6f %.6f %.6f %.6f",
            i - 1, VP[i][1], VP[i][2], VP[i][3], VP[i][4]))
    end

    -- 校驗: 主角 RootComponent 相對位置 ≈ 世界座標, 投影應落在螢幕偏下中央附近
    local root = theraohe.read_u64(onself + 0x208)
    if root then
        local rx = theraohe.read_f32(root + 0x1e4)
        local ry = theraohe.read_f32(root + 0x1e8)
        local rz = theraohe.read_f32(root + 0x1ec)
        if rx and ry and rz then
            local sx, sy, cw = project(VP, rx, ry, rz, sw, sh)
            if sx then
                print(string.format(
                    "[pubgm] pad root %.2f %.2f %.2f -> screen (%.1f, %.1f), w=%.2f",
                    rx, ry, rz, sx, sy, cw))
            else
                print("[pubgm] pad 投影失敗 (cw=0)")
            end
        end
    end

end
