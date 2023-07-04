customElements.define('pan-box', class extends HTMLElement {
    static observedAttributes = ["zoom", "min-zoom", "max-zoom", "modifier-key"]
    constructor() {
        super();
        this.s = {
            zoom: 1,
            minZoom: 0.1,
            maxZoom: Infinity,
            modifierKey: 'Shift',
            lastScroll: [0, 0],
            lastPointer: [0, 0],
        }
        this.bind(this)
        this.onWheel = e => {
            e.preventDefault()
            this.s.zoom += e.deltaY / 1000
        }
    }
    bind(element) {
        element.attachEvents = element.attachEvents.bind(element);
        element.render = element.render.bind(element);
        element.cacheDom = element.cacheDom.bind(element);
        element.onPointerDown = element.onPointerDown.bind(element);
        element.onPointerMove = element.onPointerMove.bind(element);
        element.onPointerUp = element.onPointerUp.bind(element);
    }
    render() {
        this.attachShadow({ mode: "open" });
        this.shadowRoot.innerHTML = `<style>
#viewport { height: 100%; width: 100%; overflow: auto; cursor: grab; }
#viewport.manipulating { cursor: grabbing; }
</style>
<div id="viewport" style="zoom: ${this.s.zoom};"><slot></slot></div>`
    }

    connectedCallback() {
        this.render();
        this.cacheDom();
        this.attachEvents();
    }
    cacheDom() {
        this.dom = {
            viewport: this.shadowRoot.querySelector("#viewport")
        };
        this.dom.viewport.scroll(200, 200);
    }
    attachEvents() {
        this.dom.viewport.addEventListener("wheel", this.onWheel);
        this.dom.viewport.addEventListener("pointerdown", this.onPointerDown);
    }
    onWheel(e){
        e.preventDefault();
        this.zoom += e.deltaY / 1000;
    }
    onPointerDown(e) {
        if(!this.isModifierDown(e)) return;
        e.preventDefault();
        this.dom.viewport.classList.add("manipulating");
        this.s.lastPointer = [
            e.offsetX,
            e.offsetY
        ];
        this.s.lastScroll = [
            this.dom.viewport.scrollLeft,
            this.dom.viewport.scrollTop
        ];;

        this.dom.viewport.setPointerCapture(e.pointerId);
        this.dom.viewport.addEventListener("pointermove", this.onPointerMove);
        this.dom.viewport.addEventListener("pointerup", this.onPointerUp);
    }
    onPointerMove(e) {
        const currentPointer = [
            e.offsetX,
            e.offsetY
        ];
        const delta = [
            currentPointer[0] + this.s.lastScroll[0] - this.s.lastPointer[0],
            currentPointer[1] + this.s.lastScroll[1] - this.s.lastPointer[1]
        ];

        this.dom.viewport.scroll(this.s.lastScroll[0] / this.s.zoom - delta[0] / this.s.zoom, this.s.lastScroll[1] / this.s.zoom - delta[1] / this.s.zoom, { behavior: "instant" });
    }
    onPointerUp(e) {
        this.dom.viewport.classList.remove("manipulating");
        this.dom.viewport.removeEventListener("pointermove", this.onPointerMove);
        this.dom.viewport.removeEventListener("pointerup", this.onPointerUp);
        this.dom.viewport.releasePointerCapture(e.pointerId);
    }
    isModifierDown(e){
        if(!this.s.modifierKey) return true;
        if(this.s.modifierKey === "ctrl" && e.ctrlKey) return true;
        if(this.s.modifierKey === "alt" && e.altKey) return true;
        if(this.s.modifierKey === "shift" && e.shiftKey) return true;
        return false;
    }
    attributeChangedCallback(name, oldValue, newValue) {
        this[name] = newValue;
    }
    set zoom(val){
        this.s.zoom = Math.min(Math.max(parseFloat(val), this.s.minZoom), this.s.maxZoom);
        if(this.dom && this.dom.viewport){
            this.dom.viewport.style.zoom = this.s.zoom;
        }
    }
    get zoom(){
        return this.s.zoom;
    }
    set ["min-zoom"](val){
        this.s.minZoom = val;
    }
    get ["min-zoom"](){
        return this.s.minZoom;
    }
    set ["max-zoom"](val){
        this.s.maxZoom = val;
    }
    get ["max-zoom"](){
        return this.s.maxZoom;
    }
    set ["modifier-key"](val){
        this.s.modifierKey = val;
    }
    get ["modifier-key"](){
        return this.s.modifierKey;
    }
})