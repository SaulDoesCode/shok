import d from "/domlib.js"
const {query, render, domfn} = d
const {section, div, button, span, p, header, iframe, textarea, style} = domfn
// await d.ready

/* css */
style`
.viewer {
    position: fixed;
    display: inline-flex;
    align-items: center;
    align-content: center;
    justify-content: center;
    z-index: 1000;
    top: 1mm;
    left: 30vw;
    min-height: 420px;
    height: 95%;
    width: 90%;
    min-width: 380px;
    max-width: 70vw;
    border-radius: 1mm;
}
iframe {
    position: relative;
    display: block;
    width: 100%;
    height: 70vh;
    min-height: 300px;
    box-shadow: 0 0 1mm 0.5mm rgba(0,0,0,.2);
}`

const viewer = section.viewer({$: 'body'})

document.onpointerdown = e => {
    const item = e.target
    if (item && item.tagName == 'A') {
        e.preventDefault()
        e.stopPropagation()
        view(item.href)
    }
}
document.oncontextmenu = e => {
    const item = e.target
    if (item && item.tagName == 'A') {
        e.preventDefault()
        e.stopPropagation()
        view(item.href)
    }
}
let lastSrc = ''
const view = src => {
    if (src == lastSrc) return
    viewer.innerHTML = ""
    iframe({$: viewer, src})
    lastSrc = src
}