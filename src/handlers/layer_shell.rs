// 文件: handlers/layer_shell.rs
// 作用: 处理 layer-shell 协议相关事件，管理层表面的生命周期和状态
// Wayland概念: layer-shell - 允许客户端创建在桌面不同层级显示的窗口
// Rust概念: trait实现 - 为State类型实现WlrLayerShellHandler trait

use smithay::delegate_layer_shell;
use smithay::desktop::{layer_map_for_output, LayerSurface, PopupKind, WindowSurfaceType};
use smithay::output::Output;
use smithay::reexports::wayland_server::protocol::wl_output::WlOutput;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::wayland::compositor::{get_parent, with_states};
use smithay::wayland::shell::wlr_layer::{
    self, Layer, LayerSurface as WlrLayerSurface, LayerSurfaceData, WlrLayerShellHandler,
    WlrLayerShellState,
};
use smithay::wayland::shell::xdg::PopupSurface;

// 导入本地模块
use crate::layer::{MappedLayer, ResolvedLayerRules};
use crate::niri::State;
use crate::utils::{is_mapped, output_size, send_scale_transform};

// 实现WlrLayerShellHandler trait
// 作用: 处理layer-shell协议的核心回调
impl WlrLayerShellHandler for State {
    // 函数: shell_state
    // 作用: 获取layer-shell状态管理器的可变引用
    fn shell_state(&mut self) -> &mut WlrLayerShellState {
        &mut self.niri.layer_shell_state
    }

    // 函数: new_layer_surface
    // 作用: 处理新层表面的创建
    // 参数:
    //   surface - 新的层表面
    //   wl_output - 关联的Wayland输出
    //   _layer - 表面层级
    //   namespace - 表面命名空间
    fn new_layer_surface(
        &mut self,
        surface: WlrLayerSurface,
        wl_output: Option<WlOutput>,
        _layer: Layer,
        namespace: String,
    ) {
        // 确定输出设备
        let output = if let Some(wl_output) = &wl_output {
            // 从Wayland资源获取输出实例
            Output::from_resource(wl_output)
        } else {
            // 使用当前活动输出
            self.niri.layout.active_output().cloned()
        };
        let Some(output) = output else {
            // 无可用输出则关闭表面
            warn!("no output for new layer surface, closing");
            surface.send_close();
            return;
        };

        // 获取表面资源
        let wl_surface = surface.wl_surface().clone();
        // 添加到未映射表面集合
        let is_new = self.niri.unmapped_layer_surfaces.insert(wl_surface);
        assert!(is_new);

        // 获取输出对应的层映射
        let mut map = layer_map_for_output(&output);
        // 映射新层表面
        map.map_layer(&LayerSurface::new(surface, namespace))
            .unwrap();
    }

    // 函数: layer_destroyed
    // 作用: 处理层表面的销毁
    fn layer_destroyed(&mut self, surface: WlrLayerSurface) {
        // 获取表面资源
        let wl_surface = surface.wl_surface();
        // 从未映射集合中移除
        self.niri.unmapped_layer_surfaces.remove(wl_surface);

        // 查找并移除已映射的表面
        let output = if let Some((output, mut map, layer)) =
            // 遍历所有输出查找包含此表面的层映射
            self.niri.layout.outputs().find_map(|o| {
                let map = layer_map_for_output(o);
                let layer = map
                    .layers()
                    .find(|&layer| layer.layer_surface() == &surface)
                    .cloned();
                layer.map(|layer| (o.clone(), map, layer))
            }) {
            // 从层映射中解除映射
            map.unmap_layer(&layer);
            // 从已映射集合中移除
            self.niri.mapped_layer_surfaces.remove(&layer);
            Some(output)
        } else {
            None
        };
        // 如果找到关联输出，触发重绘
        if let Some(output) = output {
            self.niri.output_resized(&output);
        }
    }

    // 函数: new_popup
    // 作用: 处理层表面弹出窗口的创建
    fn new_popup(&mut self, _parent: WlrLayerSurface, popup: PopupSurface) {
        // 解除弹出窗口约束
        self.unconstrain_popup(&PopupKind::Xdg(popup));
    }
}

// 宏: delegate_layer_shell!
// 作用: 将layer-shell协议委托给State处理
// Rust概念: 属性宏 - 自动生成协议实现代码
delegate_layer_shell!(State);

// State的扩展实现
impl State {
    // 函数: layer_shell_handle_commit
    // 作用: 处理层表面提交事件，管理表面映射状态
    // 参数: surface - 提交的Wayland表面
    // 返回: bool - 是否处理了事件
    // 流程图:
    //   [开始]
    //   -> 查找根表面
    //   -> 确定关联输出
    //   -> 如果是根表面:
    //        |-> 检查初始配置:
    //            |-> 未发送: 发送初始配置
    //            |-> 已发送: 
    //                |-> 表面已映射: 创建MappedLayer
    //                |-> 表面未映射: 移除MappedLayer
    //   -> 如果是子表面: 标记重绘
    //   -> 触发输出重排
    pub fn layer_shell_handle_commit(&mut self, surface: &WlSurface) -> bool {
        // 查找根表面
        let mut root_surface = surface.clone();
        while let Some(parent) = get_parent(&root_surface) {
            root_surface = parent;
        }

        // 查找关联输出
        let output = self
            .niri
            .layout
            .outputs()
            .find(|o| {
                let map = layer_map_for_output(o);
                map.layer_for_surface(&root_surface, WindowSurfaceType::TOPLEVEL)
                    .is_some()
            })
            .cloned();
        let Some(output) = output else {
            return false;
        };

        // 检查是否为根表面
        if surface == &root_surface {
            // 获取初始配置状态
            let initial_configure_sent = with_states(surface, |states| {
                states
                    .data_map
                    .get::<LayerSurfaceData>()
                    .unwrap()
                    .lock()
                    .unwrap()
                    .initial_configure_sent
            });

            // 获取输出层映射
            let mut map = layer_map_for_output(&output);

            // 在发送初始配置前排列层，以尊重客户端可能发送的任何尺寸
            map.arrange();

            // 获取层表面实例
            let layer = map
                .layer_for_surface(surface, WindowSurfaceType::TOPLEVEL)
                .unwrap();

            // 检查初始配置是否已发送
            if initial_configure_sent {
                // 检查表面是否已映射
                if is_mapped(surface) {
                    // 从未映射集合中移除
                    let was_unmapped = self.niri.unmapped_layer_surfaces.remove(surface);

                    // 为新映射的层表面解析规则
                    if was_unmapped {
                        // 获取配置
                        let config = self.niri.config.borrow();

                        // 计算规则
                        let rules = &config.layer_rules;
                        let rules =
                            ResolvedLayerRules::compute(rules, layer, self.niri.is_at_startup);

                        // 获取输出尺寸和缩放
                        let output_size = output_size(&output);
                        let scale = output.current_scale().fractional_scale();

                        // 创建已映射层表面
                        let mapped = MappedLayer::new(
                            layer.clone(),
                            rules,
                            output_size,
                            scale,
                            self.niri.clock.clone(),
                            &config,
                        );

                        // 添加到已映射集合
                        let prev = self
                            .niri
                            .mapped_layer_surfaces
                            .insert(layer.clone(), mapped);
                        if prev.is_some() {
                            error!("MappedLayer was present for an unmapped surface");
                        }
                    }

                    // 为按需(on-demand)表面提供焦点
                    // 一些启动器(如lxqt-runner)依赖此行为
                    // 注意:
                    //   1) 独占层表面已在update_keyboard_focus()中自动获得焦点
                    //   2) 同层级的独占层表面在update_keyboard_focus()中已优先于按需表面
                    //   https://github.com/YaLTeR/niri/issues/641
                    let on_demand = layer.cached_state().keyboard_interactivity
                        == wlr_layer::KeyboardInteractivity::OnDemand;
                    if was_unmapped && on_demand {
                        self.niri.layer_shell_on_demand_focus = Some(layer.clone());
                    }
                } else {
                    // 表面未映射
                    let was_mapped = self.niri.mapped_layer_surfaces.remove(layer).is_some();
                    // 添加到未映射集合
                    self.niri.unmapped_layer_surfaces.insert(surface.clone());

                    // 重置初始配置状态
                    if was_mapped {
                        with_states(surface, |states| {
                            let mut data = states
                                .data_map
                                .get::<LayerSurfaceData>()
                                .unwrap()
                                .lock()
                                .unwrap();
                            data.initial_configure_sent = false;
                        });
                    }
                }
            } else {
                // 首次配置: 发送缩放和变换信息
                let scale = output.current_scale();
                let transform = output.current_transform();
                with_states(surface, |data| {
                    send_scale_transform(surface, data, scale, transform);
                });

                // 发送配置事件
                layer.layer_surface().send_configure();
            }
            // 释放层映射
            drop(map);

            // 触发输出重排(内部会调用queue_redraw())
            self.niri.output_resized(&output);
        } else {
            // 处理子表面提交
            self.niri.queue_redraw(&output);
        }

        true
    }
}