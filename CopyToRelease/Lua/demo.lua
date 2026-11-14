-- TheRaohe Lua 反射示例
-- globals(没有 local) 都会反射到 Lua 頁面:
--   無參數函式 -> 按鈕; 其它值 -> 控件(bool=勾選框, 數字=DragValue, 字符串=輸入框)

enabled = true
speed = 1
name = "theraohe"

function hello()
    print("hello from lua")
end

function toggle()
    enabled = not enabled
    print("enabled = " .. tostring(enabled))
end
