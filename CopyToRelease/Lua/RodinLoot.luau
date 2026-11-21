--!strict
-- 引擎注入的全域表: cast 成 any 讓型別分析器別再報未知全域, 執行時無效果
local theraohe = (_G :: any).theraohe

-- 拾取物 ESP 測試檔: 枚舉 Level Actors, 對「非玩家 Actor」投影標籤;
-- 特別處理疑似拾取物 (旗標 + 內存佈局驗證通過) 則嘗試標名 (items.toon 有對應 id 時顯示 "M416 x30"),
-- 否則標指紋 (物理上無法區分同族藍圖物件)
--
-- 配置見文件尾部全域變量, Luau 頁可熱改 (.toon 持久化)

local module_candidates = { "libue4.so", "UE4", "Unreal", "Game", "PUBGM" }

local GWORLD_PATTERN = '[[["%s"+0xFC70A70]+0x810]+0x78]'
local CTRL_PATTERN = '[[[[[["%s"+0xFC70A70]+0x810]+0x78]+0x38]+0x78]+0x30]'
local ONSELF_PATTERN = '[[[[[[["%s"+0xFC70A70]+0x810]+0x78]+0x38]+0x78]+0x30]+0x29F8]'

local PLAYER_MARK_OFF = 0x2cd8
local PLAYER_MARK = 479.5

-- 配置: 最大標記距離 (cm), 超出不投影; 過多標籤擠屏會堆幀率
LootMaxDist = 20000.0 :: number

-- 是否顯示原始 ID (id=101004) 而非字樣; 默認 false = 解析 Loot.toon 顯示名稱/顏色/字號
ShowID = false :: boolean

-- 有 ItemDefineID 的 actor 一律畫; 沒有的 (車輛/裝飾物等) 默認不畫; true 時畫 vtbl 指紋數字
ShowFingerprint = false :: boolean

-- 黑名單: 逗號分隔 vtable 相對偏移 hex (與螢幕標籤一致, 如 "0xdb47a68, 0xdb47ae8");
-- 空 = 不剔除任何東西, 全部標注
LootExclude = "" :: string

local exclude_raw: string? = nil
local exclude_set = {}
local function sync_exclude()
    if exclude_raw == LootExclude and exclude_raw ~= nil then
        return
    end
    exclude_raw = LootExclude
    exclude_set = {}
    for part in string.gmatch(LootExclude or "", "[^,%s]+") do
        local v = tonumber(part)
        if v then
            exclude_set[v] = true
        end
    end
end

local function resolve_first(pattern: string): number?
    for _, pat in ipairs(module_candidates) do
        local p = theraohe.resolve_chain(string.format(pattern, pat))
        if p then
            return p
        end
    end
    return nil
end

-- 探針: UI/日誌都不印為真, 用於核對 current build 的 ItemDefineID 位移
LootProbe = false :: boolean

-- 依序查 Loot.toon 的各分類表 (key 必須是數字 ID); 命中就回 {name, srgba, level}
local LOOT_TABLES = { "Important", "Weapons", "Meds", "Armor", "Attachments", "Ammo", "Grenades", "Vehicles", "Misc" }

local function loot_lookup(id: number): (string?, string?, number)
    for _, t in ipairs(LOOT_TABLES) do
        local n = theraohe.keyed_tabular_objects(t, id, "name")
        if n then
            local c = theraohe.keyed_tabular_objects(t, id, "srgba")
            local lvl = theraohe.keyed_tabular_objects(t, id, "level")
            return tostring(n), c and tostring(c) or nil, tonumber(lvl) or 0
        end
    end
    return nil, nil, 0
end

-- "r,g,b,a" 字串 → 四個 0-255 整數
local function srgba_parse(s: string?): (number, number, number, number)
    if s then
        local r, g, b, a = string.match(s, "(%d+),(%d+),(%d+),(%d+)")
        if r then
            return tonumber(r) or 255, tonumber(g) or 255, tonumber(b) or 255, tonumber(a) or 255
        end
    end
    return 255, 255, 255, 220
end

-- 讀條目: 7.x Global_4.7_SDK 確認的 PickUpWrapperActor 佈局 (父類 PickUpWrapperActor 起算落在 0x5f8)
-- DefineID 的 Type 在 0x5f8 / TypeSpecificID 在 0x5fc / Count 在 0x610 / ItemId 在 0x758;
-- 物品 ID 全部是 6-7 位十進位; 落在範圍外的當垃圾/非物品 actor 處理
local function pickup_item_first(actor: number): (number?, number?, number?)
    local espec = theraohe.read_i32(actor + 0x5fc)
    local count = theraohe.read_i32(actor + 0x610)
    local itemid = theraohe.read_i32(actor + 0x758)
    local id: number? = nil
    if espec and espec >= 100000 and espec <= 9999999 then
        id = espec
    elseif itemid and itemid >= 100000 and itemid <= 9999999 then
        id = itemid
    end
    if id then
        return id, count, espec
    end
    return nil, nil, espec
end

-- (不做 Begin: 行程選擇由 Rodin.luau 統一處理, 這裡重複選擇 == 雙重 init)

local DEG = math.pi / 180

local function vec(x: number, y: number, z: number): vector
    return vector.create(x, y, z)
end
local dot = vector.dot

local function rotator_axes(pitch: number, yaw: number, roll: number): (vector, vector, vector)
    local p, y, r = pitch * DEG, yaw * DEG, roll * DEG
    local SP, CP = math.sin(p), math.cos(p)
    local SY, CY = math.sin(y), math.cos(y)
    local SR, CR = math.sin(r), math.cos(r)
    return vec(CP * CY, CP * SY, SP),
        vec(SR * SP * CY - CR * SY, SR * SP * SY + CR * CY, -SR * CP),
        vec(-(CR * SP * CY + SR * SY), SR * CY - CR * SP * SY, CR * CP)
end

local function build_view_projection(eye: vector, rot: { pitch: number, yaw: number, roll: number }, fov_deg: number, aspect: number, near: number, far: number): { { number } }
    local xscl = 1.0 / math.tan(fov_deg * DEG * 0.5)
    local yscl = xscl * aspect
    local zscl = (far + near) / (far - near)
    local zoff = 2.0 * far * near / (far - near)
    local f, r, u = rotator_axes(rot.pitch, rot.yaw, rot.roll)
    local V = {
        { r.x, r.y, r.z, -dot(r, eye) },
        { u.x, u.y, u.z, -dot(u, eye) },
        { f.x, f.y, f.z, -dot(f, eye) },
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

local function project(VP: { { number } }, p: vector, sw: number, sh: number): (number?, number?, number?)
    local x, y, z = p.x, p.y, p.z
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

local ticks: number = 0

function Tick(): ()
    local ok, err = pcall(function()
        ticks = ticks + 1
        local onself = resolve_first(ONSELF_PATTERN)
        local ctrl = resolve_first(CTRL_PATTERN)
        local gworld = resolve_first(GWORLD_PATTERN)
        if not (onself and ctrl and gworld) then
            return
        end
        local pcm = theraohe.read_u64(ctrl + 0x548)
        if not pcm or pcm == 0 then
            return
        end
        local px = theraohe.read_f32(pcm + 0x530)
        local py = theraohe.read_f32(pcm + 0x534)
        local pz = theraohe.read_f32(pcm + 0x538)
        local pitch = theraohe.read_f32(pcm + 0x548)
        local yaw = theraohe.read_f32(pcm + 0x54c)
        local roll = theraohe.read_f32(pcm + 0x550)
        local fov = theraohe.read_f32(pcm + 0x554)
        if not fov or fov < 1.0 or fov > 170.0 then
            fov = theraohe.read_f32(ctrl + 0x2b14)
        end
        if not (px and py and pz and pitch and yaw and roll and fov) then
            return
        end
        local sw, sh = theraohe.screen_size()
        if not sw then
            return
        end
        local w, h = sw, sh
        if h > w then
            w, h = h, w
        end
        local aspect = w / h
        local near, far = 10.0, 1000000.0
        local eye = vec(px, py, pz)
        local rot = { pitch = pitch, yaw = yaw, roll = roll }
        local VP = build_view_projection(eye, rot, fov, aspect, near, far)
        local base = theraohe.module_base()
        if not base or base == 0 then
            return
        end
        local level = theraohe.read_u64(gworld + 0x30)
        if not level then
            return
        end
        local arr = theraohe.read_u64(level + 0xa0)
        local num = theraohe.read_u32(level + 0xa8)
        if not arr or not num then
            return
        end
        if num > 4096 then
            return
        end

        local max_d2 = LootMaxDist * LootMaxDist
        sync_exclude()
        for i = 0, num - 1 do
            local actor = theraohe.read_u64(arr + i * 8)
            if actor and actor ~= 0 and actor ~= onself then
                local marker = theraohe.read_f32(actor + PLAYER_MARK_OFF)
                if marker ~= PLAYER_MARK then
                    local root = theraohe.read_u64(actor + 0x208)
                    local vtbl = theraohe.read_u64(actor)
                    if root and root ~= 0 and vtbl and vtbl >= base then
                        local rel = vtbl - base
                        if not exclude_set[rel] then
                            local x = theraohe.read_f32(root + 0x220)
                            local y = theraohe.read_f32(root + 0x224)
                            local z = theraohe.read_f32(root + 0x228)
                            if x and y and z then
                                local dx = x - eye.x
                                local dy = y - eye.y
                                local dz = z - eye.z
                                if dx * dx + dy * dy + dz * dz <= max_d2 then
                                    local spx, sp_y, cw = project(VP, vec(x, y, z), sw, sh)
                                    if spx and cw > 0.0 and spx >= -2000 and spx <= sw + 2000 and sp_y >= -2000 and sp_y <= sh + 2000 then
                                        -- 繪製任何有有效 ItemDefineID 的 actor: 不是拾取族也照樣標出;
                                        -- 默認解析 Loot.toon 顯示字樣+顏色+字號; ShowID=true 顯示裸 id
                                        local id, icount, espec = pickup_item_first(actor)
                                        local label: string? = nil
                                        local r, g, b, a, style = 255, 255, 255, 220, 0
                                        if id then
                                            if ShowID then
                                                label = string.format("id=%d", id)
                                            else
                                                local nm, color, lvl = loot_lookup(id)
                                                if nm then
                                                    r, g, b, a = srgba_parse(color)
                                                    style = lvl
                                                    label = nm
                                                end
                                                -- Loot.toon 沒有的，不畫
                                            end
                                            if LootProbe and ticks % 120 == 1 then
                                                print(string.format("[item] id=%d count=%s spec=%s", id, tostring(icount), tostring(espec)))
                                            end
                                        elseif ShowFingerprint then
                                            label = string.format("%d", rel)
                                        end
                                        if label then
                                            theraohe.draw_text(spx, sp_y, 0.5, 0.5,
                                                label,
                                                r, g, b, a, style)
                                        end
                                    end
                                end
                            end
                        end
                    end
                end
            end
        end
    end)
    if not ok then
        print("[loot] ERROR: " .. tostring(err))
    end
end
